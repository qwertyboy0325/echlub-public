//! Real-time audio test kit.
//!
//! Small, generic helpers the DAW crates' tests share, so each method has
//! one definition:
//!
//! - [`alloc`]: a counting global allocator that counts allocations and
//!   frees only on threads that ask to be watched, a lock that keeps
//!   counting tests in one test binary apart, and the positive control
//!   that proves the counter really counts.
//! - [`click`]: the click metric (peak third difference of a signal).
//! - [`bits`]: bit-for-bit comparison of sample buffers that names the
//!   first differing index.
//! - [`blocks`]: run a block callback over a buffer in fixed-size slices.
//!
//! The kit knows no engine type: it takes closures and sample slices, so
//! any crate can use it and it depends on nothing. A dev-dependency only.

pub mod alloc;
pub mod bits;
pub mod blocks;
pub mod click;
