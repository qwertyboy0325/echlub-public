# EchLub DAW Engine: Evidence

This document lists what the DAW engine claims and, for each claim, the test
or bench that checks it, the method, the measured result with its conditions,
and what is not measured. The engine's central claims are that the audio
thread never allocates, that output does not depend on block size, offline
versus live rendering, or worker-thread count, and that transport and graph
changes do not click. The checks work by exact comparison (bit-for-bit where
possible), a counting allocator, and a click metric with a hard-cut control
that proves the test can fail. A section near the end explains those methods
in general terms, because they apply to any real-time audio code. Everything
here was produced on one author's machines, and nobody has listened to the
result.

> **Reading this outside the private workspace.** This document comes from the
> EchLub DAW workspace; the public repository holds only a subset of it
> (`echlub-rt-testkit`, `echlub-rt-thread`, `echlub-audiofile`, `echlub-node`,
> `echlub-dsp`). Paths such as `crates/echlub-engine/...`, `crates/echlub-synth/...`,
> `crates/echlub-desktop-host/...`, `apps/desktop/...`, `docs/ai/...` and
> `scripts/...` name code and notes that are **not in this repository**, so the
> tests and benches cited from there cannot be run from here. "Decision N",
> "the work log" and the short commit hashes refer to the author's private
> design log and git history.

## 1. How to read the numbers

- **Machines.** "M4 Max" is the author's Apple M4 Max MacBook Pro. "MacBook Pro"
  is the same MacBook Pro as used for the self-tests (its speakers, 48
  kHz). The first-milestone bench says "Apple-silicon Mac" and does not name
  the chip. No other machine has been used.
- **Build.** Release builds unless stated. All sample rates are 48 kHz unless
  stated.
- **Load.** Where the work log records the machine's load average during a
  bench (other work, a virtual machine, was running), it is quoted. Benches
  that print no load average are quoted without one.
- **Three kinds of result.** *Measured* (a test or bench produced the number).
  *Simulated input only* (a simulated device, input or MIDI; no real
  hardware). *Not verified by a person* (nobody has listened; the interactive
  paths of the app have not been hand-driven).
- **Provenance.** This document did not re-run anything. Figures are quoted from
  the author's private work log, from comments recorded next to
  the tests, and from the commit messages of the commits that introduced the
  tests. Where a test asserts a limit, the limit is given separately from the
  measured value. If a fresh run disagrees with a number here, the run wins:
  the tests and the workspace's verify script are authoritative over documentation.
- **Commands.** Listed in section 6.

## 2. Guarantees, tests and results

### 2.1 Real-time safety

| Guarantee | Test or bench | Method | Result and conditions | Not measured |
| --- | --- | --- | --- | --- |
| `Engine::process` never allocates or frees, across every command and graph change | `crates/echlub-engine/tests/engine.rs`: `process_never_allocates_or_frees` | A counting global allocator with a thread-local watch flag around every `process` call. Script of 21 steps (play, click, mix, loop, seek, master gain, swaps while playing and stopped, identical swap, tracks removed, latency change, parameter, seek storms that overflow the crossfade readers, `Panic`, play, pause), 40 blocks of 512 frames after each, then 400 idle blocks. Ends by asserting output is exactly silent once idle. | 0 allocations and frees. 6 tracks including three factory presets on the synth. | Counts heap use only: not locks, syscalls or page faults. Runs the audio code on the test thread, not on a device callback. Includes a positive control (see the 2.1 note). |
| The same, with worker threads | `engine.rs`: `process_never_allocates_or_frees_on_a_worker_pool` | Same script. Three pool workers set the watch flag in their start-up hook, so they are watched for their whole life. | 0 allocations and frees. | Only 3 workers; only this machine. |
| The same for the recording paths | `crates/echlub-engine/tests/recording.rs`: `every_new_path_runs_without_allocating` | Same allocator. 13 steps (input latency, record with punch, loop, play, live notes including a late one, graph swap that changes the input, record off and on, seek, `Panic`, play, pause), 20 blocks of 256 frames each, with a live-note driver and a draining writer. | 0 allocations and frees in `process_duplex`. Asserts the late-note path ran. | As above. |
| The host callbacks never allocate | `crates/echlub-desktop-host/tests/rt_alloc.rs` | Same allocator, watching the output callback (input link, monitoring, recording, capture, a calibration in progress, then a simulated input), the input callback including a forced ring overflow, and the MIDI callback. 2000 blocks of 256 frames. **Includes a positive control**: an allocation and free inside the watched region must count 2. | 0 allocations. Asserts that more than 1000 packets were recorded, the overflow path ran, calibration returned, and no note was late. | Simulated input and a simulated MIDI source only. Not run inside a real CoreAudio callback. |

Note on the positive control: since the module split (`bfe3550`) every test
binary that uses the counting allocator takes it from `echlub-rt-testkit` and
runs a positive control (`the_allocation_counter_catches_an_allocation`, or
`alloc::positive_control()` inside the test): `echlub-engine` (`engine.rs`,
`recording.rs`, `session.rs`), `echlub-dsp`, `echlub-synth` (no allocation
after `Synth::new`), `echlub-node` (pushing into the recording ring),
`echlub-builtin-nodes` (every built-in node), `echlub-recording` (capture
writes and MIDI pushes) and `echlub-desktop-host`. Counting tests in one
binary hold an exclusive lock so they cannot count each other.

### 2.2 Determinism

| Guarantee | Test or bench | Method | Result and conditions | Not measured |
| --- | --- | --- | --- | --- |
| Output is identical for any block size | `engine.rs`: `block_size_does_not_change_a_single_sample` | A 6-track song (clips, a basic synth, a latency probe, pad, bass and mono lead presets with stolen voices) with a loop (frames 5000 to 61111) and the click on, rendered for 150,000 frames at block size 64, then compared with block sizes 17, 128, 512 and 1024. Compared bit for bit (`f32::to_bits`, through `echlub-rt-testkit::bits`, which names the first differing frame). | Identical for all five sizes. | One song; one platform. |
| The same for the synth alone, for any call slicing | `crates/echlub-synth/tests/synth_quality.rs`: `output_is_deterministic_and_independent_of_render_slicing` | A scripted sequence of notes, patch jumps and mod-wheel moves on the pad preset, rendered in slices of 1, 7, 64, 128, 1000 frames and random slices of 1 to 700. | Identical. | |
| The same with monitoring, recording and punching | `recording.rs`: `monitoring_and_recording_do_not_depend_on_the_block_size_or_threads` | Auto monitoring, a loop, a punch, a live note and an identical graph swap, at block sizes 64, 17 and 512; the output and every recorded take are compared. | Identical output and identical takes. | |
| Offline render equals live playback | `engine.rs`: `offline_render_is_the_live_engine` | The same song played live in blocks of 300 for 100,000 frames plus its latency, compared with `render_offline`. The graph must report itself deterministic. | The first `latency` frames are exactly zero; the rest equals the offline render exactly. The latency is the 300 frames of the probe. | Only deterministic graphs. |
| Output is identical for any number of worker threads | `engine.rs`: `any_number_of_threads_gives_the_same_samples` | A script of loops, seeks, a graph swap, a stop, the click, on 30 tracks, in blocks of 256 for 150,000 frames, serial executor against pools of 1, 3 and 7 workers. | Identical for all. The recording test repeats this with 1 and 3 workers. | Not run on a loaded machine or under forced preemption. Worker counts above 7 are not tested. |
| SIMD lanes equal the portable path | `echlub-synth/src/simd.rs`: `backends_agree_bit_for_bit`; `echlub-synth/src/lib.rs`: `portable_and_native_lanes_render_identically` | Unit tests comparing the NEON path against the portable reference. | Bit-identical. | Only aarch64 is measured. No claim about x86_64 or wasm output. |
| A live note equals the same note in the song | `recording.rs`: `a_live_note_sounds_exactly_like_the_same_note_in_the_song`, `live_notes_from_the_driver_queue_play_like_commands` | Compared bit for bit for block sizes 17, 256 and 1024. | Identical. | |

### 2.3 Click metric and continuity

The metric is the largest absolute third difference, `|x[n+3] - 3x[n+2] +
3x[n+1] - x[n]|`, over each channel. The limit is `1.5e-4`, a step of about
-82 dBFS. Method and reasons are in section 4.2.

All rows are from `crates/echlub-engine/tests/continuity.rs` unless noted, M4
Max, release, 256-frame blocks. Each test also measures the same moment done as
a hard cut (`Panic`) and asserts that the hard cut exceeds 100 times the limit,
so the test is shown able to see a click.

| Moment | Continuous (measured) | Hard cut (measured) |
| --- | --- | --- |
| Steady material alone (a 220 Hz tone clip plus a held sine note), no event | 1.6e-5 (asserted under a quarter of the limit) | not applicable |
| Stop, loop wrap (several), seek while playing, graph swap (clip moved, note changed; also with a patch change), swap that removes tracks, clip edges | between 1.4e-5 and 4.5e-5 across all of these (work log); the worst is the swap that moves a clip and changes a held note (the new note's attack), 4.5e-5. Per-case values are printed by the test and not recorded | 0.6 to 0.66 for every hard cut |
| Clip edges on a full-scale DC clip, two butted clips in antiphase | within the range above | unfaded, the edges would be steps of 1/sqrt(2) and a jump of 2/sqrt(2), so 1.4 to 2.8 by calculation in the test |
| Monitor switch, play, punch in and out (`recording.rs`) | 3.8e-5 (commit `c0147fc`) | a one-frame hard cut asserted above 100 times the limit |
| Latency 0 to 300 to 0 while playing (`latency.rs`) | asserted under 1.5e-4 | not measured |
| Synth: voice steal, retrigger of a sounding note (poly, mono, legato), nine parameter jumps (`echlub-synth/tests/synth_quality.rs`) | worst 7.3e-5 against the same 1.5e-4 limit (the limit is 2x the worst case) | the test prints the step an unsmoothed change would make |

The margin on the engine's worst continuous case is 1.5e-4 / 4.5e-5, about 3.3x.
Behavioural checks in the same file: the synth's 0.3 s release is still audible
100 ms after a stop and the output is exactly silent by 0.9 s; an identical
graph swap leaves the next second bit-identical to no swap; an untrimmed clip
start sounds at full gain on its first frame while its end is faded; a held
note receives its note-off on the exact loop-end frame (compared sample for
sample against the synth driven by hand).

Not measured: any click metric on material other than low smooth tones and DC
steps; listening; continuity through a real device.

### 2.4 Latency alignment

| Guarantee | Test | Method | Result | Not measured |
| --- | --- | --- | --- | --- |
| Tracks of different latency line up on the musical frame | `crates/echlub-engine/tests/latency.rs`: `tracks_of_different_latency_line_up_on_the_musical_frame` | Three tracks with an event at musical frame 10,000: a clip impulse, the same impulse behind a 300-frame look-ahead stand-in, and a synth note. Offline render. | The impulse peaks on frame 10,000 for both clip tracks, exactly one non-zero frame per channel. The synth track equals the raw synth output moved earlier by its reported latency, sample for sample (`==`). The synth's first frame above 1e-3 lands between 2 frames before and 4 frames after the note start (the attack ramps from zero). | Built-in nodes only. No third-party plugin latency. |
| The reported position is the audible one | `latency.rs`: `the_reported_position_is_the_audible_one` | 256-frame blocks with a 300-frame graph latency: position is 0 while inside the delay, then `rendered - 300`; stopped and seeking positions checked at each block. | Exact at every checked block. | |
| A swap that changes latency does not click | `latency.rs`: `a_swap_that_changes_the_latency_crossfades` | Latency 0 to 300 to 0 during playback. | Click metric asserted under 1.5e-4. | |
| A recorded input lands on the frame the performer heard | `recording.rs`: `a_recorded_impulse_lands_on_the_frame_the_performer_heard` | A simulated loopback device with a 517-frame round trip and a 300-frame graph latency. | The recorded impulse is on song frame 20,000 for 128, 256 and 512-frame blocks. Output is late by exactly the graph latency. | Simulated device only. |
| Regression found by the block-size test | `latency.rs`: `a_track_falling_idle_inside_a_block_still_goes_through_its_delay` | A stop that lands on a clip's impulse, 64 and 512-frame blocks. | Before the fix the impulse came out 300 frames early with 512-frame blocks (frame 20,000 instead of 20,300) and not with 64. Fixed in `10362c6`; the test fails without the fix. | |
| The synth's reported latency matches its measured delay | `synth_quality.rs`: `latency_matches_the_measured_delay` | Phase delay at 98 Hz by least-squares fit, then group delay versus frequency from the phase step between neighbouring semitones. | Reported latency 2 frames; measured 2.08 (the linear-phase FIR alternative: 19.50). Group delay below 1 kHz equals the phase delay to within 0.1 frame; about 2.1 frames below 2 kHz rising to about 5.5 near 20 kHz (the IIR is dispersive). | Latency under modulation and unison is not separately measured. |

### 2.5 Synth quality

All in `crates/echlub-synth/tests/synth_quality.rs` unless noted. M4 Max, release.
Nobody has listened to the synth; these figures are the only evidence.

| Property | Method | Measured | Asserted limit | Not measured |
| --- | --- | --- | --- | --- |
| Alias level of band-limited oscillators | Worst spectral component in 20 Hz to 20 kHz that is not a harmonic of the fundamental, relative to the strongest harmonic. 16,384-point FFT with a Kaiser window (beta 20). Single bare oscillator (no filter, no unison), seven held pitches with fundamentals of about 1 kHz to 7.9 kHz. Also compared with a naive oscillator and the earlier polyBLEP saw. | Worst: saw -75.0 dB, pulse -78.7 dB, triangle -102.3 dB (work log; the test comment says -102.2) | Saw and pulse -70 dB, others -95 dB; and at least 20 dB better than the naive oscillator at every pitch | Aliasing under pitch modulation, with unison and detune, with a resonant or driven filter, or in the full presets |
| Alias level of hard sync | A saw slave 19 semitones above a silent master, four master pitches | Worst -82.2 dB (note 84) | -77 dB | |
| Filter magnitude against the analytic target | Measured sine gain through the state-variable (low, band, high, notch) and ladder filters against the closed-form response of the same digital filter; internal rate 96 kHz; cutoffs 100 Hz to 15 kHz, resonance 0, 0.5, 0.9, frequencies 0.125 to 4 times cutoff | Worst error 0.0073 dB | 0.05 dB | Behaviour at resonance 1 for the state-variable filter, large-signal ladder behaviour |
| Ladder self-oscillation | Kicked by a 1e-3 impulse at resonance 1 for 10 s at 96 kHz, four cutoffs from 110 Hz to 7040 Hz | Oscillation pitch within 0.0 cents of the cutoff (as recorded in the work log) | Under 50 cents; peak under 2.0 and last-second peak above 0.3 | |
| Robustness | Fuzz with a fixed seed: 400 filter runs of 2048 samples with inputs up to +40 dB and coefficients jumping every 32 samples; 40 synths with 40 random events and patch changes each | No non-finite output | Filter output below 200 times input and ladder below 64; synth output below 1000 | Only fixed seeds |
| No subnormal arithmetic | `synth/mod.rs`: `states_never_go_subnormal` checks outputs and every filter and decimator state | None found | | CPU time was also watched in the bench for jumps as release tails decay (`engine` bench, pad held, ringing, silent); no figure is recorded in the work log |
| Decimator | `synth/decim.rs` tests | Stop band -125.3 dB; pass band flat to 6e-7 dB (commit `6d55109`) | | |

Per-voice cost, before and after the SIMD and low-latency pass (commit
`6d55109`, merged as `ef2a685`). `crates/echlub-synth/examples/synth_cost.rs`
measures nanoseconds per sounding voice per output frame for each preset with
1, 4, 8 and 16 notes, 128-frame blocks, best of 5 runs of 2 s, native SIMD
lanes against portable lanes. The work log records the ratios; the
absolute nanosecond table is printed by the example and not recorded.

| Step | Per-voice cost | Source |
| --- | --- | --- |
| The rewritten synth against the placeholder it replaced | About 9x the placeholder per voice | Work log, milestone 2 first modules |
| After SIMD lanes and the IIR decimator | 2.85x lower for Keys, 3.5x lower for Bass and Pad; the mono Lead with hard sync is about 10% slower | Work log, engine and synth pass |
| A bare-sine row added to the cost bench | Half its per-voice cost bounds what a non-oversampled render could save; no factory preset is alias-free enough to use one | Commit `7313201` |

Whole-engine shares before and after. These come from *different* harnesses and
different loads, so they are context, not a clean before and after.

| Load | Before the pass (engine bench: one core, 128-frame blocks, release; the work log names no machine for it and notes a busy machine) | After the pass (`bench_threads` on one thread, best of 3 mean, load average about 3 to 4) |
| --- | --- | --- |
| 8 voices of Keys, one track | 1.7% mean | not measured |
| 32 Keys tracks of 8 voices | 53% mean | 24.0% mean |
| 16 Pad tracks of 8 voices | 57% mean, worst blocks over 100% | not measured on its own |
| 64 audio plus 16 Pad tracks of 8 voices | not measured | 24.5% mean |
| 32 Bass tracks of 8 voices | not measured | 27.0% mean |

Wavetables were evaluated and not adopted
(`echlub-synth/src/wt_eval.rs`, ignored by default; commit `bdc0e2b`;
M4 Max, release, load average about 4, best of 5). Worst alias at pitches 84 to
119: polyBLEP (the synth) -75.0 dB; wavetable with linear reads -98.3 dB; with
4-point Hermite reads -124.5 dB. Cost per lane per 96 kHz sample: single mono
voice, polyBLEP 0.71 to 0.73 ns against 0.74 to 0.75 ns for the wavetable;
7-voice stereo unison, polyBLEP 0.71 to 0.72 ns against 0.39 ns for the
wavetable (about 1.8x cheaper). The synth keeps polyBLEP because a single
oscillator is no cheaper. The open questions stay in the test module.

### 2.6 Multicore

Bench: `crates/echlub-desktop-host/examples/bench_threads.rs`. The bench thread stands in for
the device's IO thread, plus N-1 workers, all under the real-time policy and in
one work interval of the bench's own (`AudioWorkIntervalCreate`), started and
finished every block with a one-period deadline as CoreAudio does for its IO
workgroup. Blocks of 128 frames at 48 kHz (2.67 ms budget), 3 s per run, paced
in real time so that idle workers really park and wake. M4 Max, release.

After the merge with the SIMD synth (work log): best of 3 runs, mean share
of the 128-frame block, load average about 3 to 4.

| Load | 1 thread | More threads |
| --- | --- | --- |
| 64 audio tracks plus 16 Pad tracks of 8 voices | 24.5% | 6.2% on 8 threads |
| 32 Bass tracks of 8 voices | 27.0% | 6.4% on 12 threads |
| 32 Keys tracks of 8 voices | 24.0% | 5.9% on 6 threads |

Scaling flattens at around 6 to 8 threads. Output was identical for every
thread count (see 2.2). Without the workgroup runs were bimodal, with worst
blocks of 100% to 300% of the budget.

Before the SIMD merge (commit `f420440` message, best of 3, load average about
4): 64 audio plus 16 Pad x 8 voices went 62.9% (1 thread), 32.5% (2), 18.2%
(4), 11.1% (8), 9.2% (12) with worst blocks of 14% at 12 threads; 32 Bass x 8
75.2% to 15.8% on 8 threads, flat after 10; 32 Keys 50.8% to 9.7% on 12.
Without a workgroup the same pool ran at about 50% with worst blocks over 100%,
with or without the real-time policy.

The app's self-test reported 7 of 7 workers under the real-time policy and 7 of
7 joined to the device's IO workgroup (workgroup max parallel threads 12).

Not measured: the bench inside the real device's IO workgroup (it used its own
work interval); preemption worst cases; other machines; the app under heavy
load; long sessions.

### 2.7 App self-test and first-milestone bench

`cargo run --release -p echlub-desktop -- --selftest` plays the fixture project
(three synth tracks) for 2 s on the default output and exits 0 only with sound
and no xruns. MacBook Pro speakers, 48 kHz, 256-frame buffer, release:

| Callbacks | Frames | Max load (of buffer time) | Xruns | Master peak |
| --- | --- | --- | --- | --- |
| 386 | 98,816 | 0.83% | 0 | 0.578 |

This shows the callback runs without underruns for that project. It does not
show how far the app scales.

First-milestone engine bench (placeholder synth; `cargo run --release -p
echlub-engine --example bench`; Apple-silicon Mac, one core, 128-frame blocks at
48 kHz = 2.67 ms; share of that budget, mean and worst block):

| Load | Mean | Worst |
| --- | --- | --- |
| 128 stereo audio tracks | 3.4% | 6.8% |
| 32 synth tracks x 8 voices | 6.6% | 16.9% |
| 64 audio plus 16 synth tracks | 4.9% | 9.1% |

### 2.8 Recording (simulated input only)

Every row uses a simulated device, input or MIDI source. The machine for the
timed rows is the MacBook Pro at 48 kHz, release.

| Guarantee | Test | Method | Result | Not measured |
| --- | --- | --- | --- | --- |
| A take lands on its frames; loop passes stack; punch records only its range | `recording.rs` (engine); `session::rec` unit tests in `crates/echlub-desktop-host/src/session_rec.rs` (7 tests) | Simulated loopback with a 517-frame round trip and a 300-frame graph latency; loop 10,000 to 30,000 over several passes; punch 15,000 to 25,000 | Takes of exactly the expected positions and lengths, impulse on song frame 20,000 in every pass | Any real device |
| A full ring never loses data silently | `recording.rs`: `a_full_ring_counts_every_dropped_packet` | A 4-slot ring and 100 blocks | Dropped plus kept packets equals all packets, dropped plus kept frames equals all frames, high-water mark 4 of 4 | |
| The writer survives a stalled disk | `crates/echlub-recording/src/recorder.rs`: `sixteen_tracks_survive_a_250_ms_disk_stall_with_no_loss` | 16 stereo tracks, 3 s, the writer stalled 250 ms, a ring of 1.5 s per track | 0 dropped; every take complete and sample-exact; the ring never filled | Real disk behaviour |
| A forced overrun is counted and takes stay in place | `recorder.rs`: `a_forced_overrun_is_counted_and_the_takes_stay_in_place` | The same load with a ring of 64 slots and a 300 ms stall | Packets dropped and counted; the writer's silence fill equals the dropped frames (within one packet per track at a take's end); every non-silent frame is on its song frame | |
| Crash safety of the WAV writer | `crates/echlub-audiofile/src/wavfile.rs`: `a_half_written_recording_is_repaired_to_every_whole_frame` | A writer dies after a checkpoint and more writes; the last write is torn by 6 bytes; `repair` is run | The header still says the checkpoint (3,000 frames); repair recovers 9,999 whole frames and cuts 2 torn bytes; the samples equal the input exactly; repairing again changes nothing | A crash during `repair`; disks that reorder or drop `fsync` |
| A finished file reads back exactly; foreign files are refused | `wavfile.rs`: `a_finished_recording_reads_back_exactly`, `repair_refuses_other_files` | Round trip through the engine's decoder; repair of a 16-bit PCM file | Exact; refused | |
| Crashed takes are recovered on open | `recorder.rs`: `recover_repairs_an_unfinished_take_and_keeps_finished_ones`; `session_rec.rs`: `opening_repairs_a_crashed_take_and_puts_it_back` | A finished and an unfinished take in one folder | The unfinished take is repaired and marked finished; the finished one is untouched | |
| MIDI jitter becomes a fixed offset | `crates/echlub-desktop-host/src/midi.rs`: `jitter_becomes_a_fixed_offset` | Simulated delivery jitter of 0 to 1 ms and callback jitter of 0 to 1 ms, buffers of 64, 128, 256, 512 frames | Scheduling error 0 frames at fixed offsets of 2.83, 4.17, 6.83, 12.17 ms respectively. Notes played on arrival instead scatter over more than half a buffer (asserted) | Real MIDI hardware jitter |
| Late notes are counted, never dropped | `midi.rs`: `notes_later_than_the_margin_are_counted` | Delivery slower than the margin | Late notes counted; exactly those notes are off their frame | |
| The device round trip is measured, and calibration finds hidden latency | `crates/echlub-desktop-host/src/duplex.rs` tests | A simulated device with 23 and 17 frames of unreported converter delay | Device timestamps miss the 40 hidden frames, so an uncalibrated recording lands exactly 40 frames late; after calibration it lands on the frame; calibrated lag within 0.05 frame of the true round trip | A real loopback; the author runs `--calibrate` (it plays a noise burst) |
| Calibration accuracy with noise and drift | `duplex.rs`: `calibration_accuracy_with_noise_and_drift`; `calibrate.rs` tests | Six cases with noise up to 0.1 amplitude, clock drift up to 100 ppm and fractional acoustic delays; the FFT correlation is checked against a direct one; silence or noise alone is refused | Worst error asserted under 0.5 frame (printed values are not recorded) | |
| Two clocks drift, boundedly and visibly | `duplex.rs`: `two_clocks_drift_is_bounded_counted_and_tracked` | The input 200 ppm fast for 20,000 blocks of 256 frames (about 107 s) | Frames trimmed match the drift to within one block, no overflows, round-trip estimate within 1 frame; input slow gives counted underruns | Two real devices. There is no drift resampling. The app warns and recommends an Aggregate Device |
| Always-on capture | `crates/echlub-recording/src/capture.rs` tests | Five minutes of simulated playing; keep a stretch or the last phrase | A kept stretch lands on the song frame it was played against | The menu-bar item on screen |
| Whole-app recording self-test | `--selftest-record` | 64 audio plus 16 Pad tracks playing, 8 stereo tracks recording a simulated 16-channel input for 10 s | 0 xruns, 0 dropped, recording ring high water 160 to 192 of 32,768 packets, max load 6.4% to 10.9% | A real interface; longer sessions |
| The project file round trips and bad files are refused | `crates/echlub-desktop-host/src/project_file.rs` tests | Older single-file audio entries read as one take; round trip through JSON; bad files refused rather than half loaded | Passes | |
| The host has no UI types | `apps/desktop/src/tests.rs`: `host_module_has_no_ui_types` | A source scan for `egui`, `eframe`, `rfd::` | Passes | |

### 2.9 The module split changed no output

The split into crates (commits `bfe3550` to `149c926`) was meant to change no
behaviour. Checked after every stage, on the M4 Max:

| Check | Method | Result |
| --- | --- | --- |
| Offline renders are bit-identical | SHA-256 of the WAV files written by `render_presets` (every factory preset) and by `echlub-project` from two recipes (a synthetic MIDI fixture, and a longer arrangement kept outside the repository), before the split and after each stage | Identical at every stage. |
| No test was lost | `cargo test --release` over the DAW crates | 312 passed (2 ignored) before; 336 passed (2 ignored) after. The added tests are positive controls, no-allocation tests for the new crates, the `--force` guard of `echlub-project`, and the mutable-project scan now run in both desktop crates. |
| The boundaries hold | `scripts/check-daw-architecture.py`; each rule also run once against a planted violation, which it reported | Passes. |
| The app still runs its self-tests | `--selftest` and `--selftest-session` with `--output-device none` | Pass. With no device, the real-time worker set-up is not exercised. |

Not checked: anything heard; the app on a real output device after the split.

## 3. Reading the click table

The point of the click table is the pairing. A continuous case reads 1e-5 to
5e-5; the same moment cut hard reads 0.6. That is more than 10,000 times apart.
A reviewer does not need to trust the limit, only the gap.

## 4. Method

### 4.1 A counting global allocator with a thread-local flag

The claim "the audio thread never allocates" has to hold for every code path,
including code in dependencies and the standard library. The only place that
sees all of them is the global allocator. So the tests install one that wraps
the system allocator and increments a counter on `alloc`, `dealloc` and
`realloc`.

- **Why the flag is thread-local.** The test harness, the control thread that
  prepares graphs and builds a new `GraphDesc`, and the writer drain all
  allocate legitimately, often in the same process and test. A global flag
  would count them. A thread-local flag set only around `process` (and, for
  worker pools, set once in each worker's start-up hook so workers are watched
  for their whole life) counts only what the real-time threads do.
- **Why frees count.** A `free` on the audio thread can take a lock inside the
  allocator and can be unbounded, so it is as much a violation as an
  allocation. The design returns replaced graphs to the control thread for
  exactly this reason, and the test would catch a regression.
- **A positive control.** A counter that reads zero proves nothing unless it
  can read non-zero. Every counting test binary allocates and frees a vector
  inside the watched region and asserts the count is exactly 2 (and that an
  unwatched allocation is not counted), using `echlub-rt-testkit`.
- **Coverage is a list.** The test drives a script covering every command type,
  graph swaps in each state, and the overflow cases. A new command that is not
  added to the script is not covered. That is the main weakness of the method,
  and why the script is long.
- **What it cannot see.** Locks, syscalls, page faults, priority inversion, and
  anything off the watched thread. These are not proven by this method.

### 4.2 Why the third difference is a click metric

A click is a discontinuity: a step in value, or a kink in slope. The first
difference responds to any loud low-frequency sound. The second responds to
every curved signal. The third difference of a smooth signal is tiny, because
for `A sin(wn)` with small `w` it is about `A w^3`, while a step of height `s`
produces a third difference of `2s`, and a kink produces about its slope
change. So the metric ignores smooth content and reports discontinuities,
whatever the level around them.

Practical consequences, which are the useful part:

- **The noise floor scales as frequency cubed.** A 0.5-amplitude sine at 500 Hz
  gives 1.2e-4, close to the limit, so test material is chosen to be smooth and
  low: a 220 Hz tone, a plain sine voice. A triangle wave's own corners alone
  measure 1.2e-2, so a triangle cannot be used as test material.
- **The limit is set from evidence.** `1.5e-4` is a step of about -82 dBFS
  (7.5e-5), set at twice the worst measured synth case (7.3e-5). The engine's
  worst continuous case is 4.5e-5.
- **A test that cannot fail proves nothing.** Every case also measures the same
  moment as a hard cut and asserts it is more than 100 times the limit.
- **Fade lengths can be derived from it.** For a smootherstep fade over N
  samples, the largest third difference of a full-scale step is about
  `60 / N^3`. At 3 ms and 48 kHz (N = 144) that is 2.0e-5, so two full-scale
  edges meeting stay under a third of the limit. The clip-edge fade (3 ms) is
  chosen from this calculation, as a duration short enough to keep a drum
  attack and long enough to meet the limit.
- **It is per channel and per sample.** It does not model hearing. It says
  there is no step above a known level, not that nothing is audible.

### 4.3 Why bit-identity across block sizes matters

Hosts call an engine with whatever block size the device or the user's buffer
setting gives, and that can change at run time. If the output depends on the
block size, then:

- an offline bounce differs from what was heard live;
- a bug can exist only on some devices or settings;
- a test at one block size says nothing about another;
- comparisons need tolerances, and a tolerance hides a real error as readily as
  it hides noise.

If the output is exactly the same, comparison is exact: any difference is a
bug, with no judgement about "close enough". The tests use block sizes that
are awkward on purpose: 17 is prime and unaligned with the synth's 8-frame
modulation interval, the engine's 256-frame tail-silence chunk or any
packet size.

The discipline has a design cost, and it shows in the engine. Events carry a
frame offset inside the block and nodes render between them. Parameter
smoothing runs per sample. Silence checks for a stopped track happen on fixed
engine-clock boundaries, not at block ends. Recording packets are placed by
song frame, not by block.

It found a real bug: a track that fell idle in the middle of a block skipped
its latency delay for that whole block, so a sound came out 300 frames early at
a block size of 512 and not at 64. The recording block-size test found it and the
regression test in `latency.rs` fails without the fix.

The same reasoning applies to the other two identities. Offline equal to live
means a bounce is what the user heard, and means tests can run without a device.
Thread-count identity follows from tasks that never share state and a mix done
in a fixed order on one thread.

Limit of the claim: identity is within one build on one platform (decision 3).
It is checked with `==` on `f32` vectors, so it treats `-0.0` and `0.0` as equal
and fails on any NaN. It is not a claim about cross-platform results.

### 4.4 Simulated devices with a known answer

Recording is tested against a loopback device whose round trip, hidden
converter delay, jitter, noise and drift are set by the test. The answer is
therefore known exactly, and the test asserts the engine recovers it to the
frame (or to a stated fraction of a frame). This proves the arithmetic and the
placement logic. It does not prove anything about a real device, which is why
every recording row above is marked simulated.

## 5. What this evidence does not cover

- Listening. Nobody has judged the sound, the fades or the monitoring as heard.
- The interactive app: dialogs, shortcuts, mouse seek, loop drag, zoom, faders.
- Long sessions; heavy projects inside the app; the app under heavy load.
- Other machines, phones, the web. All figures are from one author's Apple
  silicon Macs, mostly the M4 Max.
- The multicore bench inside the real device workgroup; preemption worst cases.
- Recording from a real device or microphone; a live loopback calibration;
  real MIDI hardware jitter; drift between two real devices; the menu-bar
  indicator on screen; monitoring latency as heard.
- Network timing for a clock shared between machines. No latency claim is made.
- Third-party plugin behaviour (hosting is planned, not built).
- wasm32 output. The platform-neutral DAW crates build for
  `wasm32-unknown-unknown` (a step of the workspace's verify script), but
  nothing runs them or compares their output there.

## 6. Reproducing

None of these commands were run to write this document.

| Evidence | Command |
| --- | --- |
| Engine determinism, real-time safety, latency alignment, recording (engine) | `cargo test --release -p echlub-engine --test engine --test latency --test recording -- --nocapture` |
| Continuity and click metric | `cargo test --release -p echlub-engine --test continuity -- --nocapture` |
| Synth quality | `cargo test --release -p echlub-synth --test synth_quality -- --nocapture` |
| Host callbacks allocate nothing | `cargo test --release -p echlub-desktop-host --test rt_alloc` |
| Wavetable evaluation | `cargo test --release -p echlub-synth --lib wt_eval -- --ignored --nocapture` |
| Engine bench | `cargo run --release -p echlub-engine --example bench` |
| Per-voice synth cost | `cargo run --release -p echlub-synth --example synth_cost` |
| Multicore bench (`NO_RT=1`, `NO_WG=1` for comparisons) | `cargo run --release -p echlub-desktop-host --example bench_threads` |
| App self-tests | `cargo run --release -p echlub-desktop -- --selftest` and `-- --selftest-record` |
| Module boundaries | `python3 scripts/check-daw-architecture.py` |
| Neutral crates on wasm32 | `python3 scripts/verify.py` (step "DAW neutral crates wasm build") |

## 7. Related documents

- [daw-engine.md](daw-engine.md): the architecture these tests check.
- [daw-module-map.md](daw-module-map.md): crates, dependency direction, the
  split and what the boundary script enforces.
- The author's private work log holds the decisions and the original
  evidence sections (not in this repository).
