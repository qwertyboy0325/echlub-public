//! Every other platform: no Mach ports. Every call fails (`unsupported:-1`),
//! so callers report that nothing was handed over.

use std::fmt;
use std::time::Duration;

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

const UNSUPPORTED: PortError = PortError { call: "unsupported", code: -1 };

#[derive(Debug)]
pub struct SendRight(());

impl SendRight {
    pub fn raw(&self) -> u32 {
        0
    }
}

pub struct Rendezvous(());

impl Rendezvous {
    pub fn check_in(_service: &str) -> Result<Rendezvous, PortError> {
        Err(UNSUPPORTED)
    }
    pub fn accept(&self, _pid: i32, _timeout: Duration) -> Result<SendRight, PortError> {
        Err(UNSUPPORTED)
    }
}

#[derive(Debug)]
pub enum Letter {
    Port(SendRight),
    Note(String),
}

pub struct Inbox(());

impl Inbox {
    pub fn connect(_service: &str) -> Result<Inbox, PortError> {
        Err(UNSUPPORTED)
    }
    pub fn recv(&self, _timeout: Duration) -> Result<Option<Letter>, PortError> {
        Err(UNSUPPORTED)
    }
}

pub fn send_port(_to: &SendRight, _port: &SendRight) -> Result<(), PortError> {
    Err(UNSUPPORTED)
}

pub fn send_note(_to: &SendRight, _text: &str) -> Result<(), PortError> {
    Err(UNSUPPORTED)
}
