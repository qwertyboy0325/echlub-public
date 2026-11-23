//! The real-time rule of this crate's per-block code: nothing allocates or
//! frees. Checked with the counting allocator of `echlub-rt-testkit` (and
//! its positive control) over the building blocks (gain, pan, glide,
//! peak).
//!
//!   cargo test --release -p echlub-dsp --test rt_alloc

use echlub_dsp::{db_to_gain, pan_gains, peak, Smoothed};
use echlub_rt_testkit::alloc::{self, CountingAlloc};

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

const SR: u32 = 48_000;

#[test]
fn the_allocation_counter_catches_an_allocation() {
    alloc::positive_control();
}

#[test]
fn the_building_blocks_never_allocate() {
    let _x = alloc::exclusive();
    let mut g = Smoothed::new(0.0, SR, 8.0);
    let mut x = vec![0.5f32; 1_024];
    alloc::watched(|| {
        g.set(db_to_gain(-6.0));
        let (pl, pr) = pan_gains(0.3);
        for v in x.iter_mut() {
            *v *= g.tick() * (pl + pr);
        }
        g.snap();
        std::hint::black_box(peak(&x));
    });
    assert_eq!(alloc::count(), 0);
}
