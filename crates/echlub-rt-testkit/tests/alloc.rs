//! The counting allocator installed as this binary's global allocator: the
//! positive control passes, and watched worker threads add to the count.

use echlub_rt_testkit::alloc::{self, CountingAlloc};

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

#[test]
fn the_allocation_counter_catches_an_allocation() {
    alloc::positive_control();
}

#[test]
fn watched_threads_count_and_others_do_not() {
    let _x = alloc::exclusive();
    std::thread::spawn(|| {
        alloc::watch(true);
        drop(std::hint::black_box(Box::new(7u64)));
        alloc::watch(false);
    })
    .join()
    .unwrap();
    let after_worker = alloc::count();
    let v = std::hint::black_box(vec![1u8; 64]);
    drop(v);
    assert_eq!(after_worker, 2, "a watched worker's allocation and free count");
    assert_eq!(alloc::count(), 2, "the unwatched test thread does not");
}
