# DAW renders across platforms

Status: **done (portable math).** The investigation below led to the
decision to route every transcendental function through one pure-Rust
wrapper; the conversion is in `echlub-dsp` (`math.rs`) and the crates that
call it.

## The finding

The same project rendered bit-identically on one machine (any block size,
offline or live, any thread count) but not across machines: x86-64 Linux
and aarch64 macOS gave different bits (the engine's pinned hashes were
kept per platform for that reason), and on the Mac release and debug
builds differed too. Anything that shares a project between machines
assumes every machine renders the same. Cause: the platform's C math library (below).

## What was done

- **One wrapper**, `echlub_dsp::math`, over the pure-Rust `libm` crate
  (=0.2.16, the version already in `Cargo.lock`; built `--offline`,
  nothing downloaded): sin, cos, `sin_cos` (two separate calls), tan,
  atan, atan2, exp, exp2, ln, log2, log10, powf, tanh for f32 and f64;
  `pow10` (f32: `2^(x log2 10)` in f64, rounded once); `powi` as an
  explicit square-and-multiply loop (so no build can expand it
  differently). Exactly rounded operations stay std: `sqrt`, `abs`,
  `floor`, `ceil`, `round`, `trunc`, `fract`, `mul_add`, `%`,
  `rem_euclid`, `min`, `max`, casts.
- **Every call replaced** in `echlub-dsp`, `echlub-synth` (both lanes;
  the lanes themselves have no transcendentals), `echlub-effects`,
  `echlub-sampler`, `echlub-builtin-nodes`, `echlub-engine` (sources,
  tests, examples, benches, construction-time filter and kernel design
  in f64), and the host's recipe tool (normalisation, velocity curves,
  analysis). Left on std, not on a render path or exact anyway: the
  rest of the host (test and selftest signals, the duplex latency
  measurement), `echlub-recording`'s calibration signal (a device
  measurement), `echlub-audiofile` (no dither; an AIFF rate's `2^n` is
  exact in any implementation). The tempo map has no transcendentals.
- **Enforced**: the workspace's architecture script (rule 8) fails on the
  platform's transcendentals (`.sin()`, `.powf(`, `f64::exp`, ...) in
  those crates and in the host's recipe tool, outside
  the wrapper and the probe (which measures std on purpose). Planted
  violations were caught.

### Result

- **Probe** (`identity_probe`, 108 rows: the wrapper's functions, `powi`,
  contraction, casts, subnormals, synth stages on both lanes, every
  effect) on aarch64 macOS (Apple M4 Max) against the x86-64 Linux
  reference: **108 identical, 0 differ, in release and in debug.** The
  `std.` control rows (the same functions through std, reported apart)
  still differ, 20 of 50, and only the `std.sin_cos_pair` rows differ
  between the Mac's release and debug: the platform effect is confined
  to code the DAW no longer uses.
- **Pinned renders**: one set for every machine and profile
  (plain `0xefe021a7289db36e`, with inserts `0x09d949c283f390d0`),
  reproduced on the Mac in release and debug.
- **Accuracy** (max error in ulps of the correctly rounded result,
  against a 50-digit decimal reference, 20 001 points per function and
  type, dense over the range the DSP uses; std = glibc on x86-64):

  | function | f32 libm | f32 glibc | f64 libm | f64 glibc |
  |---|---|---|---|---|
  | sin (±8π and ±3e4) | 0.501 | 0.556 | 0.746 | 0.508 |
  | cos (±8π) | 0.500 | 0.553 | 0.742 | 0.514 |
  | tan (±1.55) | 0.787 | 0.761 | 0.716 | 0.516 |
  | atan (±50) | 0.717 | 0.717 | 0.724 | 0.500 |
  | atan2 (grid ±2) | 1.197 | 1.197 | 1.271 | 0.509 |
  | exp (f32 ±80) | 0.836 | 0.501 | 0.805 | 0.503 |
  | exp2 (±120) | 0.499 | 0.499 | 0.500 | 0.500 |
  | ln (1e-30..1e30) | 0.722 | 0.648 | 0.645 | 0.499 |
  | log2 | 0.684 | 0.500 | 0.544 | 0.500 |
  | log10 | 0.759 | 1.573 | 0.535 | 1.055 |
  | tanh (±10) | 1.888 | 1.888 | 1.556 | 1.775 |
  | powf (1e-3..1e3, ±4) | 0.816 | 0.501 | 0.790 | 0.503 |
  | pow10 (−10..6) | 0.500 | 0.501 (`powf`) | 0.773 | 0.502 |
  | powi (0.5..2, n ±8) | 5.009 | 5.009 | 4.707 | 4.707 |

  `pow10` in f32 (the decibel conversions) is correctly rounded within
  the tested range (0.500 ulp); `powi` is the plain square-and-multiply
  error, the same as std's.
- **Audible level**: renders before and after (each preset, a 4-note
  chord and a lead note, 2 s; each effect on a fixed 2 s input): synth
  max |diff| −117.6 dB (Lead) to −131.6 dB (Keys) below peak, rms −124 to
  −149 dB; only the compressor changes among the effects, −136.1 dB
  (rms −150.8 dB); delay, saturation, limiter, reverb unchanged here.
- **Speed** (an AMD Ryzen 7 3700X Linux machine, release): synth cost per voice
  1–5 % lower on every preset and voice count (e.g. Pad, 16 voices,
  65.3 → 64.3 ns per voice-frame: 319 → 324 voices per core); effects
  within ±3.5 % except the compressor, +10 % (62.3 → 68.6 ns per frame,
  +6 ns, 0.03 % of a core). Per call, the hot uses got faster:
  `10^(dB/20)` 9.4 → 6.9 ns, `20 log10 x` 16.1 → 8.6 ns; `2^(x/12)` 4.6 →
  5.0 ns. The compressor's slowdown is not the calls themselves; written
  as `exp2(dB · log2(10)/20)` in f32 its gain takes 4.1 ns and the
  compressor measures 61.5 ns per frame again (within 6 ulp of 10^x).
  Not applied: it changes the compressor's bits once more.
- **Invariants**: no allocation on the audio thread, block-size and
  thread-count identity, offline = live, wasm32 build of the neutral
  crates: the existing tests and gates, green at every stage.

## The harness (the investigation)

`crates/echlub-engine/examples/identity_probe.rs`, neutral code only:

```
cargo run --release -p echlub-engine --example identity_probe
```

It compares this machine against a reference taken on x86-64 Linux
(`examples/identity_ref/x86_64-linux.txt`, embedded) and prints only what
differs:

- every std math function the DSP calls, f32 and f64, on 256 fixed
  inputs each: how many differ, the largest distance in ulps, the first
  differing input;
- `powi` (n = -3..8), `a * b + c` beside `mul_add` (contraction),
  `round`/`floor`/`ceil`/`fract`/`rem_euclid`/`min`/`max` and float-to-int
  casts on edge inputs (NaN, infinities, out of range, -0.0), subnormal
  arithmetic (flush to zero or not), `db_to_gain` and `pan_gains`;
- stage renders as 1024-frame chunk hashes: the synth (each preset, and a
  plain sine growing a saw, a filter, noise, unison, an LFO), on the
  native lanes (NEON on aarch64) and the portable ones; each effect on a
  fixed input. The first differing chunk says where a difference first
  shows.

Inputs are built from integers and bit patterns with exactly rounded
operations, so they are the same bits everywhere. On x86-64 Linux, a
release and a debug build both match the reference (110 rows).

On the Mac, run it in both profiles (with and without `--release`).

## What the code says before the comparison

- **Contraction / FMA**: Rust never fuses `a * b + c` on its own (no
  fast-math); `mul_add` is exactly rounded wherever it runs. Expected:
  no difference. The probe checks it.
- **Casts and rounding**: `as` casts are defined (saturating, NaN to 0)
  and `round`/`floor`/... are exact. Expected: no difference.
- **Subnormals**: neither Rust nor either OS turns on flush-to-zero by
  default. Expected: no difference.
- **SIMD lanes**: the synth's NEON lanes and portable lanes do the same
  IEEE operations in the same order; a synth test already checks they
  agree bit for bit on aarch64. Expected: no difference.
- **`powi`**: lowers to an LLVM intrinsic (repeated multiplies or a
  compiler-rt call); could differ in principle. The probe checks it.
- **libm transcendental functions** (`sin`, `cos`, `tan`, `exp`, `exp2`,
  `ln`, `log2`, `log10`, `powf`, `atan2`, ...): std calls the platform's C
  math library — glibc on Linux, Apple's libm on macOS. Neither promises
  correctly rounded results, and they are different code: results differ
  by 1 ulp on some inputs. **Prime suspect.** Where the DSP uses them:
  - per sample: `10^x` (`powf`) in the compressor, limiter and saturation;
    `log10` in the compressor's level detector;
  - per control tick (every few dozen samples): the synth's `exp2` (pitch,
    unison, LFO rate), `tan` (filter), `log2`;
  - at construction: every filter and kernel design in f64 (oversampler
    and decimator sinc/Kaiser kernels, the limiter's true-peak filter, the
    reverb's absorption, elliptic half-band design): `sin`, `cos`, `tan`,
    `powf`, `exp`, `log10`. One ulp in one coefficient changes every
    sample after it.

## Results: aarch64 macOS (Apple M4 Max) against x86-64 Linux

Probe at `5aec87e`, release build: 73 rows identical, 35 differ.

- **Everything exactly rounded matched**: `powi` (all n), `a * b + c`
  and `mul_add` (no contraction), rounding, casts, `min`/`max` with
  signed zeros, subnormals (no flush to zero), and f32 `sin`, `cos`,
  `exp2`, `ln`, `log2`, `sqrt` on their grids.
- **Platform libm differs, 1–2 ulp**: f32 `atan` (4/256), `atan2`
  (40/256), `exp` (1/256), `log10` (9/256, up to 2 ulp), `powf` (1/256),
  `sin` of large arguments (36/256), `tan` (14/256), `tanh` (30/256, up
  to 2 ulp); f64 `atan`, `atan2`, `cos`, `sin`, `sin_cos`, `log10`,
  `tan` (83/256, up to 2 ulp), `tanh`. EchLub's `db_to_gain` (1/256, via
  `powf`) and `pan_gains` (via `sin`/`cos`) follow.
- **The lanes are not a cause**: every synth stage differs identically on
  the NEON and the portable lanes (same first chunk, same count).
- **Stages**: all presets and the saw+filter and sine+LFO probes differ
  (first chunk 0 to 27); of the effects only the compressor (per-sample
  `log10` and `10^x`) differs on this input — delay, saturation, limiter
  and reverb match.

### Release and debug differ on the Mac: merged `sincos` calls

On the Mac, release and debug builds also differ from each other in two
rows: `pan_gains` (release 11/512 from the reference, debug 2/512) and
`synth.Pad` (release from chunk 0, debug from chunk 3).

Cause, found on x86-64 where the same transformation happens: in an
optimised build LLVM recognises `cos(a)` and `sin(a)` of the same
argument as C library calls and merges them into one `sincos` call.
The release binary calls `sincosf` in exactly two places of EchLub code:
`echlub_dsp::pan_gains` (`(a.cos(), a.sin())`) and
`echlub_synth::Voice::control`, which inlines the synth's `pan_unity`
(`(a.cos() * SQRT_2, a.sin() * SQRT_2)`, the unison spread and the
patch pan: the Pad preset spreads its unison voices). The debug binary
calls `cosf` and `sinf` separately there. glibc's `sincosf` gives the
same bits as its `sinf`/`cosf`, so x86-64 release and debug agree;
Apple's merged entry evidently does not agree with Apple's separate
`sinf`/`cosf` on every input. (On Darwin LLVM is expected to emit
`__sincosf_stret` for the merged call; not checked in either binary.)
Pad's remaining debug difference (from chunk 3) is the plain libm
difference above.

The probe's `sin_cos_pair` rows (both results of one angle used
together, added at `d43fee1`) show it directly. On the Mac, against the
x86-64 reference:

| row | release | debug |
|---|---|---|
| `sin_cos_pair.f32` | 19/512 differ (first #51) | 1/512 (first #436) |
| `sin_cos_pair.f64` | 20/512 (first #48) | 18/512 (first #48) |
| `pan_gains` | 11/512 | 2/512 |
| `synth.Pad` (both lanes) | from chunk 0, 27/47 | from chunk 3, 17/47 |

Debug still differs (Apple's separate functions against glibc); release
differs from debug (the merged call). These four are the only rows in
which the Mac's release and debug outputs differ.

So with std math a render depends on the platform's libm **and** on the
optimisation level (and so on inlining decisions: any refactor that puts
a `sin` and a `cos` of one value in one function can change bits on the
Mac).

### Does a pure-Rust implementation remove both?

Checked with `libm` 0.2.16 in a scratch crate (offline, on this machine):

- `libm::cosf(a)` and `libm::sinf(a)` of one value stay two calls to
  `libm::math::cosf::cosf` and `libm::math::sinf::sinf` in a release build
  (the std version of the same function becomes one `sincosf` call);
  `libm::sinf(0.5) + libm::powf(10.0, 0.3)` is not folded at compile
  time. LLVM only rewrites calls it recognises as C library functions by
  their symbol name; `libm`'s functions are mangled Rust functions (the
  crate exports no unmangled C symbols), so there is nothing to recognise.
- `libm`'s use of `core::intrinsics` is behind `cfg(intrinsics_enabled)`,
  set only by its `unstable-intrinsics` feature (nightly; off by default).
  Its `arch` paths (on by default) use hardware instructions only for
  exactly rounded operations (`sqrt`, `fma`, `rint`), which give the same
  bits everywhere.

So a single wrapper over `libm` (or over own functions) removes the
optimisation-level dependence as well. To be confirmed on the Mac by the
same probe built on the wrapper, release and debug.

## Options considered (before decision 25)

Measured on this x86-64 machine (one core, scalar loop, 2^20 inputs per
function; "CR" = equal to glibc's f64 result rounded to f32):

| f32 function | std (glibc) | `libm` crate 0.2.16 | own (f64 polynomial) |
|---|---|---|---|
| sin | 1 ulp max, 1.3% not CR, 5.9 ns | 1 ulp, 0.03%, 5.4 ns | 0 ulp, 0%, 13.0 ns |
| tan | 1 ulp, 3.8%, 11.4 ns | 1 ulp, 14%, 4.9 ns | 0 ulp, 0%, 14.0 ns |
| exp2 | 1 ulp, 0.07%, 5.2 ns | 1 ulp, 0.05%, 3.9 ns | 0 ulp, 0%, 10.2 ns |
| log2 | 1 ulp, 0.03%, 5.3 ns | 1 ulp, 1.1%, 8.0 ns | 0 ulp, 0%, 9.4 ns |
| log10 | 2 ulp, 3.0%, 13.6 ns | 1 ulp, 1.2%, 8.8 ns | 0 ulp, 0%, 9.6 ns |
| 10^x | 1 ulp, 0.06%, 7.8 ns | 1 ulp, 9.7%, 44 ns (`powf`) | 0 ulp, 0%, 10.1 ns |
| exp | 1 ulp, 0.07%, 7.8 ns | 1 ulp, 9.8%, 7.1 ns | — |

f64 (design time): `libm` differs from glibc on 2–10% of inputs (1 ulp),
and is as fast or faster (sin 7.7 vs 29.7 ns, exp 7.4 vs 13.5 ns) except
`pow` (46 vs 23 ns).

1. **Own implementations** of the functions the DSP needs, written with
   + − × ÷ and floor/round only (exactly rounded, so the same bits on
   every platform, wasm included). Measured: f32 `sin`, `tan`, `exp2`,
   `log2`, `log10`, `10^x` evaluated in f64 with short series matched the
   correctly rounded result on every input tested; about 1.3–2.5× the
   time of std per call. In context: the per-sample calls are a few per
   frame (≈ +2–5 ns per frame per effect), the control-tick ones are
   1/32 of that. Cost: one small module (in `echlub-dsp`) and its tests;
   f64 design-time functions (sin/cos/tan/exp/pow/log10 to near 1 ulp)
   are more work but run only at construction. Every render changes once
   (pins re-taken).
2. **The pure-Rust `libm` crate** (rust-lang/compiler-builtins, MIT, a
   port of musl's math): no download needed for evaluation — it is already
   in the workspace's `Cargo.lock` as a transitive dependency and in the
   local cargo cache; making it a direct dependency of the DSP crates is
   still a decision to take. Its only platform-specific code is for exactly
   rounded operations (sqrt, fma, rint), so its results should be the same
   bits on every platform — to be confirmed by running a probe built on it
   on both machines. As far as I know it is also what std links for these
   functions on `wasm32-unknown-unknown` (compiler-builtins provides them
   there; not verified in this investigation), in which case native
   renders would match the browser's. Accuracy 1 ulp, not
   correctly rounded; speed comparable to glibc except `powf` (5.7×
   slower: replace `10^x` by `exp2(x · log2 10)`). Cost: route every
   call through one wrapper (e.g. `echlub_dsp::math`), a lint or arch
   check against direct std math calls, re-take pins once.
3. **Exactly rounded operations only in hot paths** (tables, polynomials,
   recurrences): this is option 1 restricted to the per-sample and
   control-rate paths; the design-time f64 math would still need 1 or 2.
4. **Accept per-platform output** and keep renders local: collaboration
   would send audio (stems, frozen tracks) instead of expecting two
   machines to render the same. Cost: bandwidth and storage, and a model
   where "the same project" can sound different by ~1e-7 between machines
   (inaudible, but not bit-comparable: hashes cannot prove two renders
   agree).

Recommended and decided: 2, `libm` through one wrapper,
with `10^x` via `exp2`. Done as described at the top.
