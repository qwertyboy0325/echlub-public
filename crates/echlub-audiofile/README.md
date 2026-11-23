# echlub-audiofile

Audio files: the WAV codec, a FLAC encoder, AIFF and CAF readers and the crash-safe streaming writer for recordings with its repair step.

Part of the EchLub DAW's module split; the layering of the crates is described in
[`docs/architecture/daw-module-map.md`](../../docs/architecture/daw-module-map.md).

## What it holds

- `wav`: decode 16/24/32-bit PCM and 32-bit float, encode 16-bit PCM or 32-bit float; plain buffers plus a channel count.
- `aiff`: decode AIFF and AIFF-C (8/16/24/32-bit PCM big- or little-endian, 32/64-bit float); compressed AIFF-C is refused.
- `caf`: decode Core Audio Format with uncompressed `lpcm` (8/16/24/32-bit integer big- or little-endian, 32/64-bit float); compressed CAF (ALAC, AAC, ...) is refused with the codec's name.
- `flac`: a FLAC encoder written here (no dependency, builds for `wasm32-unknown-unknown`): 16- or 24-bit, mono or stereo, any rate a frame header can name (checked at 44.1, 48, 88.2 and 96 kHz), fixed 4096-sample blocks. STREAMINFO with the MD5 of the unencoded audio; every frame with its CRC-8 header and CRC-16; per subframe the cheapest of constant, verbatim, fixed predictors 0-4 and LPC (orders up to 12, Tukey window, quantised coefficients) with Rice residuals in the best partitioning; stereo as the cheapest of left/right, left/side, side/right and mid/side; wasted bits dropped. Not written: seek table or other metadata, escape-coded partitions, variable blocks, other depths or channel counts. `encode` takes integers; `encode_f32` / `float_to_pcm` turn floats into integers the way the 16-bit WAV writer does (clamp to full scale, scale by 2^(bits-1)-1, round half away from zero, no dither) and count the samples that were clamped.
- `md5`: RFC 1321, for FLAC's STREAMINFO.
- `wavfile`: the streaming take writer and recovery.

## Depends on

Nothing.

## Must not depend on

Any engine type. Allocates and does I/O: control and worker threads only, never the audio thread.

## Check it

`cargo test -p echlub-audiofile` (round trips, 24/32-bit PCM, AIFF formats, every truncation of an AIFF and a CAF file; MD5 against the RFC's vectors; FLAC against a decoder for exactly the subset it writes, in the test module only: bit-exact round trips on silence, full-scale and quiet sines, white noise, a sweep, identical and opposite stereo channels, DC, square waves at the extremes, wasted bits, one sample up to odd lengths around the block size, every CRC and the MD5 checked, any flipped bit caught).

Two tests are `#[ignore]`d because they need things the repository does not have; run them by hand with `--release -- --ignored --nocapture`:

- `external_decoders_agree_sample_for_sample`: writes every generated signal as FLAC (16 and 24 bit, 44.1/48/88.2/96 kHz) and decodes it with `ffmpeg` (the path in `ECHLUB_FFMPEG`, else Homebrew's default location, else the `PATH`) and with macOS's `afconvert`; both must give the source's samples, `ffmpeg -v error -i f.flac -f null -` must print nothing, and ffmpeg's MD5 of the decoded samples must equal STREAMINFO's.
- `demo_mix_ratio_and_round_trip`: `ECHLUB_FLAC_DEMO_WAV=<a rendered mix>` (only read): sizes, encoding time and the round trip on real material.
