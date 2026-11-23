//! Real-time threads for audio on macOS: the Mach
//! time-constraint policy, the CoreAudio device's IO-thread workgroup
//! (`kAudioDevicePropertyIOThreadOSWorkgroup` + `os_workgroup_join`, macOS
//! 11+), work intervals of one's own (`AudioWorkIntervalCreate`, macOS 12+),
//! the performance-core count, and (`port`) handing a workgroup's Mach send
//! right to another process (`os_workgroup_copy_port` /
//! `os_workgroup_create_with_port`, over a bootstrap name).
//!
//! Every call fails safe: a thread that cannot get the policy or join a
//! workgroup still runs, and the caller is told. On other platforms the
//! same API exists and does nothing (no workgroup, no policy), so code that
//! uses it builds everywhere. No dependency; the system frameworks are
//! linked through FFI.

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;
#[cfg(not(target_os = "macos"))]
#[path = "fallback.rs"]
mod imp;

/// Handing a Mach send right to a child process (macOS; fails elsewhere).
#[cfg(target_os = "macos")]
#[path = "macos_port.rs"]
pub mod port;
#[cfg(not(target_os = "macos"))]
#[path = "fallback_port.rs"]
pub mod port;

pub use imp::{make_realtime, set_time_constraint, sysctl_u32, time_constraint, JoinToken, WorkInterval, Workgroup};

/// Performance cores of this machine (`hw.perflevel0.physicalcpu` on
/// Apple silicon), or `None` where unknown.
pub fn perf_cores() -> Option<usize> {
    sysctl_u32("hw.perflevel0.physicalcpu").map(|n| n as usize)
}
