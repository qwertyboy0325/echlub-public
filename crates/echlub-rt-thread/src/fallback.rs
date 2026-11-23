//! Every other platform: nothing to join, no real-time policy; callers
//! fall back to ordinary threads.

pub struct Workgroup;
pub struct JoinToken;
pub struct WorkInterval {
    pub group: std::sync::Arc<Workgroup>,
}
impl WorkInterval {
    pub fn new(_name: &str, _period_ns: u64) -> Option<WorkInterval> {
        None
    }
    pub fn joined(&self) -> bool {
        false
    }
    pub fn start(&self) {}
    pub fn finish(&self) {}
}
impl Workgroup {
    pub fn default_output() -> Option<Workgroup> {
        None
    }
    pub fn of_device(_dev: u32) -> Option<Workgroup> {
        None
    }
    pub fn max_parallel(&self) -> i32 {
        -1
    }
    pub fn join(&self) -> Option<Box<JoinToken>> {
        None
    }
    pub fn try_join(&self) -> Result<Box<JoinToken>, i32> {
        Err(-1)
    }
    pub fn copy_port(&self) -> Result<crate::port::SendRight, i32> {
        Err(-1)
    }
    pub fn from_port(_name: &str, _port: &crate::port::SendRight) -> Option<Workgroup> {
        None
    }
    pub fn leave(&self, _t: &mut JoinToken) {}
}
pub fn make_realtime(_period_ns: u64) -> bool {
    false
}
pub fn set_time_constraint(_period_ns: u64, _computation_ns: u64, _constraint_ns: u64) -> Result<(), i32> {
    Err(-1)
}
pub fn time_constraint() -> Option<(u64, u64, u64)> {
    None
}
pub fn sysctl_u32(_name: &str) -> Option<u32> {
    None
}
