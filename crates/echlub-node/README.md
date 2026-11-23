# echlub-node

The node interface every sound source and effect implements (CLAP-shaped: prepare on the control thread, process per block with sorted sample-accurate events), and the recording packet and ring types the audio thread and the recording crate share (`io`).

Part of the EchLub DAW's module split; the layering of the crates is described in
[`docs/architecture/daw-module-map.md`](../../docs/architecture/daw-module-map.md).
The engine that implements and hosts these nodes is not part of this repository; this crate is the contract between the two.

## What it holds

- `Node`, `NodeFactory`, events, parameters, latency reporting.
- `io`: `RecPacket`, `LiveNote`, `MidiRec`, `InputRun`, `RecTap` / `RecTapReader` and `rec_tap`: the types the engine and the recording code share, so that neither depends on the other.

## Depends on

`rtrb`.

## Must not depend on

The engine, recording, files, devices, UI. Platform-neutral: builds for `wasm32-unknown-unknown`.

## Check it

`cargo test -p echlub-node` (`tests/io.rs`: pushing into the ring never allocates, with a positive control).
