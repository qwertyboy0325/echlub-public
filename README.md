# EchLub: real-time audio building blocks

Five small Rust crates from the engine of a native digital audio workstation, plus the
architecture and evidence documents that explain how they are tested.

繁體中文簡介：這是 EchLub 音訊引擎的一個公開子集，包含五個互相獨立的 Rust crate：
即時音訊的測試工具（計數式配置器與 click 指標）、macOS 即時執行緒、音訊檔案
（含自行實作的 FLAC 編碼器與當機後可修復的 WAV 錄音寫入器）、節點介面，以及跨平台
結果一致的 DSP 基礎函式。文件與程式碼以英文為主。

The crates have no dependencies on each other at run time and build on their own. Outside
`rtrb` (a lock-free ring buffer) and `libm` (portable math) they use only `std`.

## What is in here

| Crate | What it is | What is technically interesting |
| --- | --- | --- |
| [`echlub-rt-testkit`](crates/echlub-rt-testkit) | Test helpers for real-time audio code. A dev-dependency only. | A counting global allocator that counts only on threads that ask to be watched, shipped with a positive control (a test that proves the counter can see an allocation, so "zero allocations" cannot pass by accident). A click metric (peak third difference) and a bit-for-bit buffer comparison that names the first differing frame. |
| [`echlub-rt-thread`](crates/echlub-rt-thread) | Real-time threads on macOS: time-constraint policy, joining an audio device's IO workgroup, work intervals, performance-core count. | Direct FFI to Mach, CoreAudio and libdispatch with the ownership of every right stated where it is taken. A module that hands a workgroup's Mach send right to a child process over a bootstrap name and checks the sender of the first message by audit token. |
| [`echlub-audiofile`](crates/echlub-audiofile) | Audio files on plain sample buffers: WAV, AIFF and AIFF-C, and CAF readers, a WAV writer with TPDF dither, a FLAC encoder, and a crash-safe streaming WAV writer with a repair step. | The FLAC encoder is written from scratch (fixed and LPC predictors up to order 12, Rice partitioning, the four stereo modes, CRC-8/16, MD5 in STREAMINFO) and tested for bit-exact round trips against a decoder written in the test module for exactly the subset it emits. The streaming writer rewrites and syncs its header at checkpoints, so a file whose writer died can be repaired to every whole frame that reached the disk. |
| [`echlub-node`](crates/echlub-node) | The interface every sound source and effect implements (shaped after CLAP: preparation on the control thread, sample-accurate sorted events per block, latency and tail reporting), plus the plain types that cross between the audio thread and the rest of a host through lock-free rings. | A "signal tap" that lets a UI observe an effect's input and output without ever being able to change the sound: single-producer single-consumer rings built from atomics, no `unsafe`, no lock, no allocation on the audio thread. |
| [`echlub-dsp`](crates/echlub-dsp) | Decibels, constant-power pan, a glide that settles exactly, cubic resampling, a radix-2 FFT, and `math`: the transcendental functions every other crate calls. | `math` wraps the pure-Rust `libm` crate so that the same input gives the same bits on x86-64 Linux and on Apple silicon, in release and debug builds, instead of whatever the platform's C library returns. The reasoning and the measurements are in [`docs/architecture/daw-cross-platform-identity.md`](docs/architecture/daw-cross-platform-identity.md). |

Every crate has its own README with what it holds, what it may depend on and how to check it.

## Documents

- [`docs/architecture/daw-evidence.md`](docs/architecture/daw-evidence.md): each claim the engine
  makes, the test or bench that checks it, the method, the measured result with its conditions
  and what is not measured. Section 4 describes the testing methods in general terms (a counting
  allocator with a positive control, bit-exact comparison, a click metric with a hard-cut
  control); they apply to any real-time audio code.
- [`docs/architecture/daw-engine.md`](docs/architecture/daw-engine.md): the structure and invariants
  of the engine these crates belong to, described by what it guarantees and how that is tested.
- [`docs/architecture/daw-cross-platform-identity.md`](docs/architecture/daw-cross-platform-identity.md):
  why the same project rendered differently on two platforms, and what was done about it.
- [`docs/architecture/daw-module-map.md`](docs/architecture/daw-module-map.md): how the crates are
  layered.

The engine itself, the synthesizer, the effects and the application are **not** in this
repository, so most tests and measurements that the evidence document cites cannot be run from
here; the document marks the paths it cites. Treat its figures as the author's measurements on
the author's machines, not as something this repository reproduces. No latency claim is made.

## Try it in a browser

The application these crates come from also runs as a WebAssembly build. The link is in this
repository's About box (the website field at the top right of the GitHub page).

It opens with a demo song. You can play it, change sounds and effects, write notes, and save
or open projects in the page. It has been checked in desktop Chrome; Safari and phones have not
been verified. The source of that build is not in this repository.

## Build and test

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.96.0; `rustup` installs it on first use).

```sh
cargo build --all-targets
cargo test --release
```

Use `--release` for tests: a few of them run millions of math calls and are very slow in debug
builds. Two tests in `echlub-audiofile` are `#[ignore]`d because they need external tools
(`ffmpeg`, macOS `afconvert`) or a user-supplied file; they are described in that crate's README.

Platform notes:

- `echlub-rt-thread` only does something on macOS (macOS 12 or later for work intervals). On
  every other platform it compiles to stubs: its functions report "not available" and the tests
  assert exactly that. The macOS paths are therefore only exercised by running the tests on a Mac.
- `echlub-audiofile`, `echlub-node`, `echlub-dsp` and `echlub-rt-testkit` use no
  platform-specific code and are intended to build for `wasm32-unknown-unknown`.
- Development and the figures in the documents come from Apple silicon Macs and one x86-64 Linux
  machine. Windows has not been tried.
- `.github/workflows/ci.yml` builds and tests on Linux.

## Notes

- This repository is a subset exported from a private monorepo by a script that copies a fixed
  list of files and checks the result; it has no history of its own beyond the exports. Issues are
  welcome, but a pull request may be applied by making the same change in the monorepo and
  exporting again, so it may not be merged as submitted.
- The code was written with AI assistance, under the author's direction and review.
- Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
  at your option.
