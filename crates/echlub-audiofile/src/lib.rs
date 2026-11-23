//! Audio files for the EchLub DAW: the WAV codec
//! ([`wav`]: 16/24/32-bit PCM and 32-bit float in, 16-bit PCM or 32-bit
//! float out), an AIFF / AIFF-C reader ([`aiff`]: uncompressed PCM and
//! float), a CAF reader ([`caf`]: uncompressed `lpcm` only) and the crash-safe streaming writer for recordings with its
//! repair step ([`wavfile`]).
//!
//! Plain buffers in and out; no engine type, no dependency. Allocates and
//! does file I/O: for control and worker threads, never the audio thread.
//! The codec builds for `wasm32-unknown-unknown` (the writer compiles there
//! but has no file system to use).

pub mod aiff;
pub mod caf;
pub mod flac;
pub mod md5;
pub mod wav;
pub mod wavfile;
