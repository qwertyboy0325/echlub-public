//! The ring types: a tap holds what it was sized for, a packet exposes only
//! its used samples, and pushing and popping never allocate (counting
//! allocator of `echlub-rt-testkit`, with its positive control).

use echlub_node::io::{rec_tap, MidiRec, RecPacket, PACKET_FRAMES};
use echlub_rt_testkit::alloc::{self, CountingAlloc};

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

#[test]
fn the_allocation_counter_catches_an_allocation() {
    alloc::positive_control();
}

#[test]
fn a_packet_shows_only_its_used_samples() {
    let mut p = RecPacket { frames: 3, channels: 2, ..RecPacket::default() };
    p.data[..6].copy_from_slice(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    assert_eq!(p.samples(), &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    assert_eq!(p.data.len(), PACKET_FRAMES * 2);
    assert!(format!("{p:?}").contains("frames: 3"), "debug leaves the samples out");
}

#[test]
fn the_tap_rings_hold_their_slots_and_never_allocate() {
    let _x = alloc::exclusive();
    let (mut tap, mut reader) = rec_tap(4, 2);
    let note = MidiRec { track: Some(1), pitch: 60, vel: 0.5, on: true, heard: 10, pos: 20, playing: true, recording: false, take: 3 };
    let (mut packets, mut notes) = (0, 0);
    alloc::watched(|| {
        for k in 0..6u64 {
            let ok = tap.packets.push(RecPacket { track: k, frames: 1, ..RecPacket::default() }).is_ok();
            packets += ok as usize;
            notes += tap.notes.push(note).is_ok() as usize;
        }
        while let Ok(p) = reader.packets.pop() {
            std::hint::black_box(p.samples());
        }
        while reader.notes.pop().is_ok() {}
    });
    assert_eq!((packets, notes), (4, 2), "a full ring refuses, never grows");
    assert_eq!(alloc::count(), 0);
}
