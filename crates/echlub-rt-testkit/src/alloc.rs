//! A counting global allocator.
//!
//! Install it in a test binary:
//!
//! ```text
//! #[global_allocator]
//! static A: echlub_rt_testkit::alloc::CountingAlloc = echlub_rt_testkit::alloc::CountingAlloc;
//! ```
//!
//! Only threads that are *watched* ([`watch`], [`watched`]) count, so the
//! test harness and the control side of a test do not. Every allocation,
//! free and reallocation on a watched thread adds one to a counter shared
//! by all threads (worker threads can watch themselves for their whole
//! life). Tests in one binary run in parallel; a counting test holds
//! [`exclusive`] for its whole run so no other counting test (or the
//! positive control) adds to its count.
//!
//! The positive control ([`positive_control`]) watches one allocation and
//! its free and checks that the counter saw exactly those two: without it
//! a test that expects zero would also pass with the allocator not
//! installed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

static COUNT: AtomicUsize = AtomicUsize::new(0);
static EXCLUSIVE: Mutex<()> = Mutex::new(());

thread_local! {
    static WATCH: Cell<bool> = const { Cell::new(false) };
}

/// The system allocator, counting on watched threads.
pub struct CountingAlloc;

#[inline]
fn note() {
    // `try_with`: the thread-local may be gone while a thread shuts down.
    if WATCH.try_with(|w| w.get()).unwrap_or(false) {
        COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every call is forwarded to the system allocator unchanged; the
// bookkeeping touches only an atomic and a const-initialised thread-local
// (neither allocates).
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        System.realloc(p, l, n)
    }
}

/// Watch (or stop watching) the calling thread.
pub fn watch(on: bool) {
    WATCH.with(|w| w.set(on));
}

/// Run `f` with the calling thread watched.
pub fn watched<R>(f: impl FnOnce() -> R) -> R {
    watch(true);
    let r = f();
    watch(false);
    r
}

/// Allocations and frees counted since the last reset.
pub fn count() -> usize {
    COUNT.load(Ordering::Relaxed)
}

/// The right to count alone in this test binary, held until dropped. The
/// counter starts from zero.
pub struct Exclusive {
    _guard: MutexGuard<'static, ()>,
}

/// Take the counting lock (waits for any other counting test in this
/// binary) and reset the counter.
pub fn exclusive() -> Exclusive {
    // A counting test that failed poisons the lock; the next one still runs.
    let guard = EXCLUSIVE.lock().unwrap_or_else(|e| e.into_inner());
    COUNT.store(0, Ordering::Relaxed);
    Exclusive { _guard: guard }
}

/// The counter counts: one watched allocation and its free count exactly
/// two. Call it from a `#[test]` in every binary that installs
/// [`CountingAlloc`]; it panics if the allocator is not installed.
#[track_caller]
pub fn positive_control() {
    let _x = exclusive();
    watched(|| drop(std::hint::black_box(vec![0u8; 16])));
    let n = count();
    assert_eq!(n, 2, "the counting allocator must see one allocation and one free (is it the #[global_allocator]?)");
    // Nothing unwatched counts.
    drop(std::hint::black_box(vec![0u8; 16]));
    assert_eq!(count(), 2, "an unwatched allocation was counted");
    COUNT.store(0, Ordering::Relaxed);
}
