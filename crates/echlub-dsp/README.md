# echlub-dsp

Signal-processing building blocks: decibels, constant-power pan, a glide that settles exactly, peak, cubic resampling, and `math`: the transcendental functions (sin, cos, tan, atan, atan2, exp, exp2, ln, log2, log10, powf, tanh, pow10, powi) every DAW crate uses, portable: the same bits on every platform.

Part of the EchLub DAW's module split; the layering of the crates is described in
[`docs/architecture/daw-module-map.md`](../../docs/architecture/daw-module-map.md).

## What it holds

- Per-block functions take buffers the caller owns and never allocate; what allocates (resampling a file) says so.
- `examples/`: `math_cost` (per-call cost of `math` beside the platform's std) and `timestretch_study`, an offline, allocating research prototype that compares four time-stretch algorithms (not real-time code; see its header).

## Depends on

`libm` (=0.2.16, pure Rust: a port of musl's math; hardware only for exactly rounded operations).

## Must not depend on

I/O, clocks, threads. Platform-neutral: builds for `wasm32-unknown-unknown`. `math` gives the same bits on x86-64 Linux and aarch64 macOS in release and debug builds (the cross-platform probe, both machines, both profiles); see [`docs/architecture/daw-cross-platform-identity.md`](../../docs/architecture/daw-cross-platform-identity.md).

## Check it

`cargo test -p echlub-dsp` (`tests/rt_alloc.rs`, with a positive control).
