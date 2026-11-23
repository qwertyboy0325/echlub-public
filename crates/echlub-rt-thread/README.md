# echlub-rt-thread

Real-time threads on macOS: the time-constraint policy, joining the device's IO workgroup, work intervals of one's own, and the performance-core count. Stubs that do nothing elsewhere, so callers build everywhere.

Part of the EchLub DAW's module split; the layering of the crates is described in
[`docs/architecture/daw-module-map.md`](../../docs/architecture/daw-module-map.md).

## What it holds

- `make_realtime`, `set_time_constraint`, `time_constraint`, `Workgroup` (with `try_join`, `copy_port`, `from_port`), `JoinToken`, `WorkInterval`, `perf_cores`, `sysctl_u32`.
- `port`: handing a workgroup's Mach send right to a child process over a bootstrap name checked in for that child (`Rendezvous` on the host, `Inbox` in the child, the hello's sender checked by its audit token), and the letters sent after it (`send_port`, `send_note`). Elsewhere every call fails with `unsupported:-1`.

## Depends on

Nothing (system frameworks through FFI).

## Must not depend on

An audio engine, a device layer or a thread pool: those belong to the code that uses this crate (the lookup of a device's workgroup and the worker pool are the caller's job).

## Check it

`cargo test -p echlub-rt-thread`. On macOS the tests give a thread the time-constraint policy and read it back, create and join a work interval, and hand a workgroup's Mach port from one end of a bootstrap name to the other inside one process. On every other platform the same tests check that each call reports "not available".
