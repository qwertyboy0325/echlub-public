//! The macOS implementation (Mach, CoreAudio, AudioToolbox and libdispatch
//! `os_workgroup` calls through FFI).

use std::ffi::{c_char, c_void, CString};

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
pub struct JoinToken {
    sig: u32,
    opaque: [u8; 36],
}

#[repr(C)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

#[repr(C)]
struct TimeConstraintPolicy {
    period: u32,
    computation: u32,
    constraint: u32,
    preemptible: u32,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioWorkIntervalCreate(name: *const c_char, clock: u32, attr: *mut c_void) -> *mut c_void;
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(id: u32, addr: *const PropertyAddress, qsize: u32, q: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
}

unsafe extern "C" {
    fn os_workgroup_join(wg: *mut c_void, token: *mut JoinToken) -> i32;
    fn os_workgroup_copy_port(wg: *mut c_void, port_out: *mut u32) -> i32;
    fn os_workgroup_create_with_port(name: *const c_char, port: u32) -> *mut c_void;
    fn os_workgroup_leave(wg: *mut c_void, token: *mut JoinToken);
    fn os_workgroup_max_parallel_threads(wg: *mut c_void, attr: *mut c_void) -> i32;
    fn os_release(obj: *mut c_void);
    fn os_workgroup_interval_start(wg: *mut c_void, start: u64, deadline: u64, data: *mut c_void) -> i32;
    fn os_workgroup_interval_finish(wg: *mut c_void, data: *mut c_void) -> i32;
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    fn pthread_self() -> *mut c_void;
    fn pthread_mach_thread_np(t: *mut c_void) -> u32;
    fn thread_policy_set(thread: u32, flavor: u32, info: *mut u32, count: u32) -> i32;
    fn thread_policy_get(thread: u32, flavor: u32, info: *mut u32, count: *mut u32, get_default: *mut i32) -> i32;
    fn sysctlbyname(name: *const c_char, old: *mut c_void, oldlen: *mut usize, new: *const c_void, newlen: usize) -> i32;
}

use crate::port::SendRight;

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}
const SYSTEM_OBJECT: u32 = 1;
const DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
const IO_WORKGROUP: u32 = fourcc(b"oswg");
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const ELEMENT_MAIN: u32 = 0;
const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;

/// An `os_workgroup_t` we hold a reference to.
pub struct Workgroup(*mut c_void);
// SAFETY: os_workgroup objects are thread-safe and reference counted;
// joining from other threads is how they are meant to be used.
unsafe impl Send for Workgroup {}
unsafe impl Sync for Workgroup {}

impl Workgroup {
    /// The IO-thread workgroup of the default output device (the one the
    /// audio backend opened), or `None` (older macOS, no device, an error).
    pub fn default_output() -> Option<Workgroup> {
        unsafe {
            let mut dev: u32 = 0;
            let mut size = 4u32;
            let a = PropertyAddress { selector: DEFAULT_OUTPUT, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN };
            if AudioObjectGetPropertyData(SYSTEM_OBJECT, &a, 0, std::ptr::null(), &mut size, &mut dev as *mut u32 as *mut c_void) != 0 || dev == 0 {
                return None;
            }
            Self::of_device(dev)
        }
    }

    /// The IO-thread workgroup of CoreAudio device `dev`.
    pub fn of_device(dev: u32) -> Option<Workgroup> {
        unsafe {
            let mut wg: *mut c_void = std::ptr::null_mut();
            let mut size = std::mem::size_of::<*mut c_void>() as u32;
            let a = PropertyAddress { selector: IO_WORKGROUP, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN };
            if AudioObjectGetPropertyData(dev, &a, 0, std::ptr::null(), &mut size, &mut wg as *mut *mut c_void as *mut c_void) != 0 || wg.is_null() {
                return None;
            }
            Some(Workgroup(wg))
        }
    }

    pub fn max_parallel(&self) -> i32 {
        unsafe { os_workgroup_max_parallel_threads(self.0, std::ptr::null_mut()) }
    }

    /// Join from the calling thread; `None` if the OS refused. The
    /// token stays at one address (boxed) until `leave`.
    pub fn join(&self) -> Option<Box<JoinToken>> {
        self.try_join().ok()
    }

    /// [`join`](Self::join) with the reason when the OS refused: the errno
    /// `os_workgroup_join` returned (`EALREADY`: the thread is in a
    /// workgroup this one does not nest with; `EINVAL`: cancelled).
    pub fn try_join(&self) -> Result<Box<JoinToken>, i32> {
        let mut t = Box::new(JoinToken { sig: 0, opaque: [0; 36] });
        // SAFETY: `self.0` is a workgroup we hold a reference to; the token
        // is ours, boxed so it keeps its address until `leave`.
        match unsafe { os_workgroup_join(self.0, &mut *t) } {
            0 => Ok(t),
            e => Err(e),
        }
    }

    /// A send right representing this workgroup, for another process
    /// (`os_workgroup_copy_port`, macOS 11). The right is the caller's (it
    /// is released when the [`SendRight`] drops). `Err`: the errno.
    pub fn copy_port(&self) -> Result<SendRight, i32> {
        let mut port: u32 = 0;
        // SAFETY: `self.0` is a workgroup we hold a reference to; the call
        // writes one `mach_port_t` into `port` and, on success, gives us
        // one reference on that send right, which `SendRight` now owns.
        match unsafe { os_workgroup_copy_port(self.0, &mut port) } {
            0 if port != 0 => Ok(SendRight::adopt(port)),
            0 => Err(-1),
            e => Err(e),
        }
    }

    /// The workgroup another process sent as a send right made by
    /// [`copy_port`](Self::copy_port) (`os_workgroup_create_with_port`). The
    /// new object has no members (the calling thread does not join) and no
    /// reference on `port` is consumed (the caller still releases it).
    /// `None`: the system refused.
    pub fn from_port(name: &str, port: &SendRight) -> Option<Workgroup> {
        let name = CString::new(name).ok()?;
        // SAFETY: a NUL-terminated name we own for the call and a send right
        // we hold; the call returns a retained object (released in `drop`)
        // or NULL.
        let wg = unsafe { os_workgroup_create_with_port(name.as_ptr(), port.raw()) };
        (!wg.is_null()).then_some(Workgroup(wg))
    }

    /// Leave, on the thread that joined.
    pub fn leave(&self, t: &mut JoinToken) {
        unsafe { os_workgroup_leave(self.0, t) }
    }
}

impl Drop for Workgroup {
    fn drop(&mut self) {
        unsafe { os_release(self.0) }
    }
}

const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;

/// A work interval of our own (`AudioWorkIntervalCreate`, macOS 12+):
/// what a thread that is not CoreAudio's IO thread uses to tell the OS
/// its period and deadline. For benches that stand in for the device.
pub struct WorkInterval {
    pub group: std::sync::Arc<Workgroup>,
    token: Option<Box<JoinToken>>,
    period_abs: u64,
}

impl WorkInterval {
    /// Created and joined by the calling thread (the one that will
    /// call `start` / `finish` every period).
    pub fn new(name: &str, period_ns: u64) -> Option<WorkInterval> {
        let name = CString::new(name).ok()?;
        let wg = unsafe { AudioWorkIntervalCreate(name.as_ptr(), OS_CLOCK_MACH_ABSOLUTE_TIME, std::ptr::null_mut()) };
        if wg.is_null() {
            return None;
        }
        let group = std::sync::Arc::new(Workgroup(wg));
        let token = group.join();
        let mut tb = TimebaseInfo { numer: 0, denom: 0 };
        unsafe { mach_timebase_info(&mut tb) };
        let period_abs = period_ns * tb.denom.max(1) as u64 / tb.numer.max(1) as u64;
        Some(WorkInterval { group, token, period_abs })
    }
    pub fn joined(&self) -> bool {
        self.token.is_some()
    }
    /// A period starts now; its deadline is one period away.
    pub fn start(&self) {
        let now = unsafe { mach_absolute_time() };
        unsafe { os_workgroup_interval_start(self.group.0, now, now + self.period_abs, std::ptr::null_mut()) };
    }
    pub fn finish(&self) {
        unsafe { os_workgroup_interval_finish(self.group.0, std::ptr::null_mut()) };
    }
}

impl Drop for WorkInterval {
    fn drop(&mut self) {
        if let Some(mut t) = self.token.take() {
            self.group.leave(&mut t);
        }
    }
}

/// Put the calling thread under the time-constraint policy for a
/// period of `period_ns`: up to half of it computing, all of it as the
/// deadline, preemptible (Apple's guidance for audio worker threads).
pub fn make_realtime(period_ns: u64) -> bool {
    set_time_constraint(period_ns, period_ns / 2, period_ns).is_ok()
}

/// Mach absolute-time units per nanosecond, as (numer, denom), or `None`.
fn timebase() -> Option<(u64, u64)> {
    let mut tb = TimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `mach_timebase_info` writes the two fields of a struct we own.
    (unsafe { mach_timebase_info(&mut tb) } == 0 && tb.numer != 0 && tb.denom != 0).then_some((tb.numer as u64, tb.denom as u64))
}

/// The time-constraint policy with explicit numbers (ns) for the calling
/// thread, preemptible. `Err` carries the kernel's `kern_return_t` (or -1
/// when the timebase is unknown): the thread keeps its old policy.
pub fn set_time_constraint(period_ns: u64, computation_ns: u64, constraint_ns: u64) -> Result<(), i32> {
    let (numer, denom) = timebase().ok_or(-1)?;
    let to_abs = |ns: u64| (ns * denom / numer).min(u32::MAX as u64) as u32;
    let mut p = TimeConstraintPolicy { period: to_abs(period_ns), computation: to_abs(computation_ns), constraint: to_abs(constraint_ns), preemptible: 1 };
    // SAFETY: the calling thread's own port (no reference taken, nothing
    // to release) and a policy struct of the flavor's size (4 words).
    let kr = unsafe {
        let port = pthread_mach_thread_np(pthread_self());
        thread_policy_set(port, THREAD_TIME_CONSTRAINT_POLICY, &mut p as *mut TimeConstraintPolicy as *mut u32, 4)
    };
    if kr == 0 {
        Ok(())
    } else {
        Err(kr)
    }
}

/// The calling thread's time-constraint policy as the kernel holds it:
/// (period, computation, constraint) in ns, or `None` when the thread has
/// the default policy (no real-time standing) or the query failed.
pub fn time_constraint() -> Option<(u64, u64, u64)> {
    let (numer, denom) = timebase()?;
    let mut p = TimeConstraintPolicy { period: 0, computation: 0, constraint: 0, preemptible: 0 };
    let (mut count, mut get_default) = (4u32, 0i32);
    // SAFETY: the calling thread's own port; the kernel writes at most
    // `count` (4) words into the struct and one `boolean_t` (an int).
    let kr = unsafe {
        let port = pthread_mach_thread_np(pthread_self());
        thread_policy_get(port, THREAD_TIME_CONSTRAINT_POLICY, &mut p as *mut TimeConstraintPolicy as *mut u32, &mut count, &mut get_default)
    };
    if kr != 0 || get_default != 0 {
        return None;
    }
    let to_ns = |a: u32| a as u64 * numer / denom;
    Some((to_ns(p.period), to_ns(p.computation), to_ns(p.constraint)))
}

pub fn sysctl_u32(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let mut v: u32 = 0;
    let mut len = std::mem::size_of::<u32>();
    let ok = unsafe { sysctlbyname(name.as_ptr(), &mut v as *mut u32 as *mut c_void, &mut len, std::ptr::null(), 0) } == 0;
    ok.then_some(v)
}
