//! Handing a Mach send right (a workgroup's port) to another process of the
//! same user (macOS).
//!
//! The way: a **bootstrap name**. The host checks a fresh, unique service name
//! in (`bootstrap_check_in`: a receive right registered in the user's launchd
//! namespace), starts the child with that name, and waits for one message. The
//! child looks the name up (`bootstrap_look_up`), makes a receive right of its
//! own (its inbox) and sends a hello carrying a send right to the inbox. The
//! host checks the hello's audit token (the sender's pid must be the child it
//! started; anything else is destroyed and ignored), keeps the send right and
//! drops the rendezvous (its receive right destroyed, the name dead). From then
//! on the host sends the child letters: a workgroup's send right
//! ([`send_port`]) or a short note ([`send_note`]: "no workgroup", and why).
//!
//! Ownership of every right is stated where it is taken; each is held by one
//! of [`SendRight`] / `ReceiveRight` and released when that drops.

use std::ffi::{c_char, CString};
use std::fmt;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Header {
    bits: u32,
    size: u32,
    remote: u32,
    local: u32,
    voucher: u32,
    id: i32,
}

/// `mach_msg_port_descriptor_t` (user space, 12 bytes): name, pad1, then a
/// word with pad2 (16 bits), disposition (8) and type (8), low bits first.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PortDesc {
    name: u32,
    pad1: u32,
    bits: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PortMsg {
    h: Header,
    count: u32,
    desc: PortDesc,
}

const NOTE_MAX: usize = 48;

#[repr(C)]
#[derive(Clone, Copy)]
struct NoteMsg {
    h: Header,
    len: u32,
    text: [u8; NOTE_MAX],
}

/// Room for the largest message we accept plus the audit trailer (52 bytes).
#[repr(C, align(8))]
struct RecvBuf([u32; 64]);

unsafe extern "C" {
    static mach_task_self_: u32;
    static bootstrap_port: u32;
    fn mach_port_allocate(task: u32, right: u32, name: *mut u32) -> i32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
    fn mach_port_mod_refs(task: u32, name: u32, right: u32, delta: i32) -> i32;
    fn mach_msg(msg: *mut Header, option: i32, send_size: u32, rcv_size: u32, rcv_name: u32, timeout: u32, notify: u32) -> i32;
    fn mach_msg_destroy(msg: *mut Header);
    fn bootstrap_check_in(bp: u32, name: *const c_char, sp: *mut u32) -> i32;
    fn bootstrap_look_up(bp: u32, name: *const c_char, sp: *mut u32) -> i32;
}

/// `audit_token_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    val: [u32; 8],
}

#[link(name = "bsm")]
unsafe extern "C" {
    fn audit_token_to_pid(token: AuditToken) -> i32;
}

const MACH_PORT_RIGHT_RECEIVE: u32 = 1;
const MACH_MSG_TYPE_PORT_SEND: u32 = 17; // MOVE_SEND: what a received send right says
const MACH_MSG_TYPE_COPY_SEND: u32 = 19;
const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;
const MACH_MSG_PORT_DESCRIPTOR: u32 = 0;
const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
const MACH_SEND_MSG: i32 = 0x1;
const MACH_RCV_MSG: i32 = 0x2;
const MACH_SEND_TIMEOUT: i32 = 0x10;
const MACH_RCV_TIMEOUT: i32 = 0x100;
/// `MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0) |
/// MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT)`.
const RCV_AUDIT_TRAILER: i32 = 3 << 24;
const MACH_RCV_TIMED_OUT: i32 = 0x1000_4003;
const AUDIT_TRAILER_SIZE: u32 = 52;
/// Longest bootstrap name (`name_t` is 128 chars with the NUL).
const NAME_MAX: usize = 127;

const ID_HELLO: i32 = 0x4543_0001;
const ID_PORT: i32 = 0x4543_0002;
const ID_NOTE: i32 = 0x4543_0003;

/// How long a send may wait for room in the receiver's queue.
const SEND_TIMEOUT_MS: u32 = 200;

fn task() -> u32 {
    // SAFETY: `mach_task_self_` is libSystem's constant for this task's
    // own port, set before `main`; reading it takes no reference.
    unsafe { mach_task_self_ }
}

/// A failed call and the code it returned (`kern_return_t`, a bootstrap
/// error, or -1 for a check of ours); shown as `call:code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortError {
    pub call: &'static str,
    pub code: i32,
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.call, self.code)
    }
}

fn check(call: &'static str, code: i32) -> Result<(), PortError> {
    if code == 0 {
        Ok(())
    } else {
        Err(PortError { call, code })
    }
}

/// One user reference on a Mach send right, ours, released on drop.
#[derive(Debug)]
pub struct SendRight(u32);

// SAFETY: a port name is a number valid in the whole task; sending on it
// and releasing it are thread-safe kernel calls.
unsafe impl Send for SendRight {}
unsafe impl Sync for SendRight {}

impl SendRight {
    /// Take ownership of one reference on send right `name`.
    pub(crate) fn adopt(name: u32) -> SendRight {
        SendRight(name)
    }
    pub fn raw(&self) -> u32 {
        self.0
    }
}

impl Drop for SendRight {
    fn drop(&mut self) {
        // SAFETY: we own exactly one user reference on this send right.
        // A failure (the name already dead) leaves nothing to release.
        let _ = unsafe { mach_port_deallocate(task(), self.0) };
    }
}

/// A receive right of ours, destroyed on drop (which also kills a
/// bootstrap name checked in on it).
struct ReceiveRight(u32);

impl ReceiveRight {
    fn allocate() -> Result<ReceiveRight, PortError> {
        let mut name = 0u32;
        // SAFETY: writes one port name; on success the receive right is ours.
        check("mach_port_allocate", unsafe { mach_port_allocate(task(), MACH_PORT_RIGHT_RECEIVE, &mut name) })?;
        Ok(ReceiveRight(name))
    }
}

impl Drop for ReceiveRight {
    fn drop(&mut self) {
        // SAFETY: we own this receive right; dropping its only reference
        // destroys the port (senders then see it dead).
        let _ = unsafe { mach_port_mod_refs(task(), self.0, MACH_PORT_RIGHT_RECEIVE, -1) };
    }
}

/// What arrived in a receive buffer.
enum Got {
    TimedOut,
    Msg,
}

fn receive(port: u32, buf: &mut RecvBuf, timeout: Duration) -> Result<Got, PortError> {
    let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let size = std::mem::size_of::<RecvBuf>() as u32;
    // SAFETY: the buffer is ours, aligned, and `size` bytes long; the
    // kernel writes at most that (a larger message fails with
    // MACH_RCV_TOO_LARGE and is destroyed by the kernel).
    let kr = unsafe { mach_msg(buf.0.as_mut_ptr() as *mut Header, MACH_RCV_MSG | MACH_RCV_TIMEOUT | RCV_AUDIT_TRAILER, 0, size, port, ms, 0) };
    match kr {
        0 => Ok(Got::Msg),
        MACH_RCV_TIMED_OUT => Ok(Got::TimedOut),
        e => Err(PortError { call: "mach_msg-receive", code: e }),
    }
}

fn header(buf: &RecvBuf) -> Header {
    // SAFETY: the buffer starts with a header the kernel wrote.
    unsafe { *(buf.0.as_ptr() as *const Header) }
}

/// The send right a received complex message carries (one port
/// descriptor), if it has exactly that shape. The right is then ours.
fn carried_send_right(buf: &RecvBuf) -> Option<u32> {
    let h = header(buf);
    if h.bits & MACH_MSGH_BITS_COMPLEX == 0 || (h.size as usize) < std::mem::size_of::<PortMsg>() {
        return None;
    }
    // SAFETY: the message is at least a `PortMsg` long (checked above).
    let m = unsafe { *(buf.0.as_ptr() as *const PortMsg) };
    let disposition = (m.desc.bits >> 16) & 0xff;
    let kind = m.desc.bits >> 24;
    (m.count == 1 && kind == MACH_MSG_PORT_DESCRIPTOR && disposition == MACH_MSG_TYPE_PORT_SEND && m.desc.name != 0).then_some(m.desc.name)
}

/// The sender's pid from the audit trailer the kernel appended.
fn sender_pid(buf: &RecvBuf) -> Option<i32> {
    let h = header(buf);
    let at = (h.size as usize).div_ceil(4);
    let words = &buf.0;
    // Trailer: type, size, seqno, sender (2 words), audit token (8 words).
    if at + 13 > words.len() || words[at + 1] < AUDIT_TRAILER_SIZE {
        return None;
    }
    let mut token = AuditToken { val: [0; 8] };
    token.val.copy_from_slice(&words[at + 5..at + 13]);
    // SAFETY: a plain function of the token's words.
    Some(unsafe { audit_token_to_pid(token) })
}

fn destroy(buf: &mut RecvBuf) {
    // SAFETY: a message the kernel just delivered into our buffer;
    // `mach_msg_destroy` releases every right it carries (and nothing else).
    unsafe { mach_msg_destroy(buf.0.as_mut_ptr() as *mut Header) }
}

fn c_name(service: &str) -> Result<CString, PortError> {
    if service.len() > NAME_MAX {
        return Err(PortError { call: "name-too-long", code: -1 });
    }
    CString::new(service).map_err(|_| PortError { call: "name-nul", code: -1 })
}

/// The host's end of the hand-over: a service name checked in for one child.
pub struct Rendezvous {
    recv: ReceiveRight,
}

impl Rendezvous {
    /// Check `service` in (it must be new and at most 127 bytes).
    pub fn check_in(service: &str) -> Result<Rendezvous, PortError> {
        let name = c_name(service)?;
        let mut port = 0u32;
        // SAFETY: `bootstrap_port` is libSystem's (set before `main`); the
        // call writes one port name: on success a receive right, ours.
        check("bootstrap_check_in", unsafe { bootstrap_check_in(bootstrap_port, name.as_ptr(), &mut port) })?;
        Ok(Rendezvous { recv: ReceiveRight(port) })
    }

    /// Wait up to `timeout` for the hello of process `pid` and return the
    /// send right to its inbox (ours). Messages from anyone else, or of
    /// another shape, are destroyed (their rights released) and ignored.
    pub fn accept(&self, pid: i32, timeout: Duration) -> Result<SendRight, PortError> {
        let end = Instant::now() + timeout;
        let mut buf = RecvBuf([0; 64]);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(PortError { call: "accept-timeout", code: -1 });
            }
            match receive(self.recv.0, &mut buf, left)? {
                Got::TimedOut => return Err(PortError { call: "accept-timeout", code: -1 }),
                Got::Msg => {
                    let ok = header(&buf).id == ID_HELLO && sender_pid(&buf) == Some(pid);
                    match carried_send_right(&buf) {
                        Some(right) if ok => return Ok(SendRight(right)),
                        _ => destroy(&mut buf),
                    }
                }
            }
        }
    }
}

/// A letter from the host to a child.
#[derive(Debug)]
pub enum Letter {
    /// A workgroup's send right (from `Workgroup::copy_port`), ours now.
    Port(SendRight),
    /// No workgroup, and why (`no-workgroup`, `device-lost`, ...).
    Note(String),
}

/// The child's end: its own receive right, whose send right went to the host.
pub struct Inbox {
    recv: ReceiveRight,
}

impl Inbox {
    /// Look `service` up and send the host a send right to a new inbox.
    pub fn connect(service: &str) -> Result<Inbox, PortError> {
        let name = c_name(service)?;
        let mut host = 0u32;
        // SAFETY: as in `check_in`; on success one reference on a send
        // right to the host's rendezvous, owned by `host` below.
        check("bootstrap_look_up", unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &mut host) })?;
        let host = SendRight(host);
        let recv = ReceiveRight::allocate()?;
        let mut m = PortMsg {
            h: Header { bits: MACH_MSG_TYPE_COPY_SEND | MACH_MSGH_BITS_COMPLEX, size: std::mem::size_of::<PortMsg>() as u32, remote: host.0, local: 0, voucher: 0, id: ID_HELLO },
            count: 1,
            // MAKE_SEND: the kernel makes a new send right from our receive
            // right and gives it to the host (our receive right stays ours).
            desc: PortDesc { name: recv.0, pad1: 0, bits: MACH_MSG_TYPE_MAKE_SEND << 16 | MACH_MSG_PORT_DESCRIPTOR << 24 },
        };
        // SAFETY: a well-formed message of `size` bytes we own; COPY_SEND
        // leaves our reference on `host` (released when it drops).
        check("mach_msg-hello", unsafe { mach_msg(&mut m.h, MACH_SEND_MSG | MACH_SEND_TIMEOUT, m.h.size, 0, 0, SEND_TIMEOUT_MS, 0) })?;
        Ok(Inbox { recv })
    }

    /// The next letter, waiting up to `timeout` (`Ok(None)`: none came).
    pub fn recv(&self, timeout: Duration) -> Result<Option<Letter>, PortError> {
        let mut buf = RecvBuf([0; 64]);
        match receive(self.recv.0, &mut buf, timeout)? {
            Got::TimedOut => Ok(None),
            Got::Msg => {
                let h = header(&buf);
                if h.id == ID_PORT {
                    if let Some(right) = carried_send_right(&buf) {
                        return Ok(Some(Letter::Port(SendRight(right))));
                    }
                } else if h.id == ID_NOTE && h.bits & MACH_MSGH_BITS_COMPLEX == 0 && h.size as usize >= std::mem::size_of::<NoteMsg>() {
                    // SAFETY: a simple message at least a `NoteMsg` long.
                    let n = unsafe { *(buf.0.as_ptr() as *const NoteMsg) };
                    let len = (n.len as usize).min(NOTE_MAX);
                    return Ok(Some(Letter::Note(String::from_utf8_lossy(&n.text[..len]).into_owned())));
                }
                destroy(&mut buf);
                Err(PortError { call: "unexpected-message", code: h.id })
            }
        }
    }
}

/// Send `port` (a copy of our send right: we keep ours) to `to`.
pub fn send_port(to: &SendRight, port: &SendRight) -> Result<(), PortError> {
    let mut m = PortMsg {
        h: Header { bits: MACH_MSG_TYPE_COPY_SEND | MACH_MSGH_BITS_COMPLEX, size: std::mem::size_of::<PortMsg>() as u32, remote: to.0, local: 0, voucher: 0, id: ID_PORT },
        count: 1,
        desc: PortDesc { name: port.0, pad1: 0, bits: MACH_MSG_TYPE_COPY_SEND << 16 | MACH_MSG_PORT_DESCRIPTOR << 24 },
    };
    // SAFETY: a well-formed message we own; both rights are COPY_SEND, so
    // our references stay ours. On a send failure the kernel returns the
    // rights to us untouched (we never moved any).
    check("mach_msg-send-port", unsafe { mach_msg(&mut m.h, MACH_SEND_MSG | MACH_SEND_TIMEOUT, m.h.size, 0, 0, SEND_TIMEOUT_MS, 0) })
}

/// Send a short note (at most 48 bytes, longer is cut) to `to`.
pub fn send_note(to: &SendRight, text: &str) -> Result<(), PortError> {
    let mut n = NoteMsg { h: Header { bits: MACH_MSG_TYPE_COPY_SEND, size: std::mem::size_of::<NoteMsg>() as u32, remote: to.0, local: 0, voucher: 0, id: ID_NOTE }, len: 0, text: [0; NOTE_MAX] };
    let b = text.as_bytes();
    let len = b.len().min(NOTE_MAX);
    n.text[..len].copy_from_slice(&b[..len]);
    n.len = len as u32;
    // SAFETY: a well-formed simple message we own; COPY_SEND keeps our
    // reference on `to`.
    check("mach_msg-send-note", unsafe { mach_msg(&mut n.h, MACH_SEND_MSG | MACH_SEND_TIMEOUT, n.h.size, 0, 0, SEND_TIMEOUT_MS, 0) })
}
