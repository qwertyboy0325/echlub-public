# echlub-rt-testkit

Shared test helpers for the DAW crates' real-time tests; a dev-dependency only, never shipped.

Part of the EchLub DAW's module split; the layering of the crates is described in
[`docs/architecture/daw-module-map.md`](../../docs/architecture/daw-module-map.md).

## What it holds

- `alloc`: a counting global allocator that counts only on watched threads, an exclusive lock per test binary, and `positive_control()`, which proves the counter really counts.
- `click`: the click metric (peak third difference).
- `bits`: bit-for-bit buffer comparison (`to_bits`) that names the first differing index and frame.
- `blocks`: run a processor over a buffer in blocks.

## Depends on

Nothing.

## Must not depend on

Nothing in the workspace at runtime: every user takes it as a dev-dependency.

## Check it

`cargo test -p echlub-rt-testkit` (`tests/alloc.rs` checks the counter itself).
