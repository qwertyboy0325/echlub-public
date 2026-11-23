//! Signal taps: the audio thread hands a copy of an effect's signal to the
//! control / UI side, as an observer that can never change what is played.
//!
//! A [`TapPoint`] belongs to one insert of the project (the host keeps one
//! per insert and gives the same `Arc` to every factory it builds for it).
//! The effect node writes into it from `process`; one [`TapReader`] attaches
//! and drains it.
//!
//! - **Untapped cost**: [`TapPoint::writer`] is one relaxed load of
//!   `armed` and a return.
//! - **Two streams**: the *audio* stream (raw pre and post frames, one record
//!   per block with the song position of its first frame) and the *level*
//!   stream (a [`LevelRec`] every [`LEVEL_FRAMES`] frames). Their storage is
//!   taken on the reader's first attach (control or UI thread); the audio
//!   thread never allocates.
//! - **Rings** are single-producer single-consumer queues of atomic slots
//!   (release on the write index after the data, acquire on the read side):
//!   no `unsafe`, no lock, no new dependency. A full ring drops the block or
//!   record and counts it; nothing waits.
//! - **One writer at a time**: a node takes a serial number when it is
//!   created ([`TapPoint::register`]); an instance whose serial is below the
//!   newest that has written does not write (a node that rings out beside its
//!   replacement at a graph swap), and the claim (one `compare_exchange`,
//!   released by [`TapWriter`]'s drop, also on unwind) keeps two writers
//!   apart for the block in which the two overlap. Neither ever waits.
//!
//! Platform-neutral: `std` atomics and `OnceLock` only.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Stream bit: raw pre and post audio frames.
pub const AUDIO: u32 = 1;
/// Stream bit: level records every [`LEVEL_FRAMES`] frames.
pub const LEVELS: u32 = 2;
/// Frames in one level record's interval: 32, a fixed boundary of the
/// effect's own frame count (1.5 kHz at 48 kHz).
pub const LEVEL_FRAMES: usize = 32;
/// Frames the audio ring holds (a power of two): 0.34 s at 48 kHz.
pub const AUDIO_FRAMES: usize = 16_384;
/// Blocks the audio ring remembers at once.
pub const AUDIO_BLOCKS: usize = 512;
/// Records the level ring holds: 5.5 s at 48 kHz.
pub const LEVEL_SLOTS: usize = 8_192;

const AUDIO_CHANNELS: usize = 4; // pre L, pre R, post L, post R
const META_WORDS: usize = 3;
const LEVEL_WORDS: usize = 8;
const FLAG_PLAYING: u32 = 1;
const FLAG_GAP: u32 = 2;

struct AudioRing {
    data: Box<[AtomicU32]>,
    /// Per block: first frame (ring index), song position, frames | flags << 32.
    meta: Box<[AtomicU64]>,
    /// Frames written (writer's; the data's own end).
    w: AtomicU64,
    /// Frames read (reader's).
    r: AtomicU64,
    /// Blocks written: the commit point (release).
    mw: AtomicU64,
    /// Blocks read.
    mr: AtomicU64,
    /// The next record to be written follows dropped blocks.
    gap: AtomicBool,
}

impl AudioRing {
    fn new() -> Self {
        AudioRing {
            data: (0..AUDIO_FRAMES * AUDIO_CHANNELS).map(|_| AtomicU32::new(0)).collect(),
            meta: (0..AUDIO_BLOCKS * META_WORDS).map(|_| AtomicU64::new(0)).collect(),
            w: AtomicU64::new(0),
            r: AtomicU64::new(0),
            mw: AtomicU64::new(0),
            mr: AtomicU64::new(0),
            gap: AtomicBool::new(false),
        }
    }
}

struct LevelRing {
    words: Box<[AtomicU32]>,
    w: AtomicU64,
    r: AtomicU64,
    gap: AtomicBool,
}

impl LevelRing {
    fn new() -> Self {
        LevelRing { words: (0..LEVEL_SLOTS * LEVEL_WORDS).map(|_| AtomicU32::new(0)).collect(), w: AtomicU64::new(0), r: AtomicU64::new(0), gap: AtomicBool::new(false) }
    }
}

/// One level record: what the effect did over `frames` frames (at most
/// [`LEVEL_FRAMES`]; fewer for the first interval after a reader attached).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LevelRec {
    /// Song position of the first frame (held while the transport is
    /// stopped).
    pub pos: u64,
    pub frames: u16,
    pub playing: bool,
    /// Records were dropped before this one.
    pub gap: bool,
    /// Sample peak of the input and of the output (either channel, linear).
    pub in_peak: f32,
    pub out_peak: f32,
    /// Sum of squares of the input and of the output over both channels
    /// and the interval (the mean square is this over `2 * frames`).
    pub in_sq: f32,
    pub out_sq: f32,
    /// The effect's meter value at the interval's end (a compressor's or
    /// limiter's gain reduction in dB, 0 or above; 0 for an effect without
    /// one).
    pub gr_db: f32,
}

/// One block of the audio stream, as [`AudioDrain`] holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioBlock {
    /// Index of its first frame in the drain's vectors.
    pub at: usize,
    pub frames: usize,
    /// Song position of its first frame (held while stopped).
    pub pos: u64,
    /// The transport was playing at its first frame (a play or stop that
    /// reaches the node inside the block, delayed by the latency upstream of
    /// it, shows from the next block on).
    pub playing: bool,
    /// Blocks were dropped before this one: its audio does not follow the
    /// previous block's.
    pub gap: bool,
}

/// Audio blocks taken from a tap: the frames of every block one after the
/// other, pre and post, left and right, and a record per block. Reuse one
/// and `clear` it.
#[derive(Debug, Clone, Default)]
pub struct AudioDrain {
    pub blocks: Vec<AudioBlock>,
    pub pre: [Vec<f32>; 2],
    pub post: [Vec<f32>; 2],
}

impl AudioDrain {
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.pre.iter_mut().chain(self.post.iter_mut()).for_each(Vec::clear);
    }
    pub fn frames(&self) -> usize {
        self.pre[0].len()
    }
}

/// What a tap counted since it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TapStats {
    /// Audio blocks and frames dropped because the ring was full.
    pub dropped_blocks: u64,
    pub dropped_frames: u64,
    /// Level records dropped because the ring was full.
    pub dropped_levels: u64,
    /// Blocks an older instance of the node did not write (a newer one had).
    pub superseded: u64,
    /// Blocks not written because another writer held the claim.
    pub contended: u64,
}

/// Why a reader could not attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// A reader is attached already.
    Busy,
    /// No stream asked for.
    NoStreams,
}

/// The tap of one insert. See the module docs.
pub struct TapPoint {
    armed: AtomicU32,
    claim: AtomicBool,
    live: AtomicU64,
    serials: AtomicU64,
    reader: AtomicBool,
    audio: OnceLock<AudioRing>,
    levels: OnceLock<LevelRing>,
    dropped_blocks: AtomicU64,
    dropped_frames: AtomicU64,
    dropped_levels: AtomicU64,
    superseded: AtomicU64,
    contended: AtomicU64,
    /// The insert's meter value (f32 bits), as its live node last stored it.
    meter: AtomicU32,
}

impl fmt::Debug for TapPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TapPoint").field("armed", &self.armed.load(Ordering::Relaxed)).field("stats", &self.stats()).finish()
    }
}

impl TapPoint {
    /// A tap point with nothing attached and no ring storage yet.
    pub fn new() -> Arc<TapPoint> {
        Arc::new(TapPoint {
            armed: AtomicU32::new(0),
            claim: AtomicBool::new(false),
            live: AtomicU64::new(0),
            serials: AtomicU64::new(0),
            reader: AtomicBool::new(false),
            audio: OnceLock::new(),
            levels: OnceLock::new(),
            dropped_blocks: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            dropped_levels: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
            contended: AtomicU64::new(0),
            meter: AtomicU32::new(0),
        })
    }

    /// Control thread: a node made for this insert takes its serial (1, 2,
    /// ... in creation order, which is graph preparation order).
    pub fn register(&self) -> u64 {
        self.serials.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The streams a reader wants now (0: none). One relaxed load.
    #[inline]
    pub fn armed(&self) -> u32 {
        self.armed.load(Ordering::Relaxed)
    }

    /// Audio thread: the insert's meter value after a block (a compressor's
    /// or limiter's gain reduction in dB). Stable across graph rebuilds, as
    /// the point is, unlike a meter owned by a factory.
    #[inline]
    pub fn set_meter(&self, v: f32) {
        self.meter.store(v.to_bits(), Ordering::Relaxed);
    }

    /// The insert's meter value as of its live node's last block.
    pub fn meter(&self) -> f32 {
        f32::from_bits(self.meter.load(Ordering::Relaxed))
    }

    pub fn stats(&self) -> TapStats {
        TapStats {
            dropped_blocks: self.dropped_blocks.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            dropped_levels: self.dropped_levels.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
            contended: self.contended.load(Ordering::Relaxed),
        }
    }

    /// Audio thread, once per `process` call: the writer for this block, if
    /// a reader wants anything and this instance (by `serial`) is the newest
    /// that writes and nobody else holds the claim. Never waits.
    #[inline]
    pub fn writer(&self, serial: u64) -> Option<TapWriter<'_>> {
        let streams = self.armed.load(Ordering::Relaxed);
        if streams == 0 {
            return None;
        }
        self.writer_slow(serial, streams)
    }

    #[cold]
    #[inline(never)]
    fn writer_slow(&self, serial: u64, streams: u32) -> Option<TapWriter<'_>> {
        if self.live.fetch_max(serial, Ordering::AcqRel) > serial {
            self.superseded.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if self.claim.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            self.contended.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Some(TapWriter { p: self, streams })
    }

    /// Control or UI thread: attach as the reader of `streams` ([`AUDIO`],
    /// [`LEVELS`], or both). Takes the rings' storage the first time,
    /// discards what is in them and arms the point; the reader's drop
    /// disarms it. One reader at a time.
    pub fn attach(self: &Arc<Self>, streams: u32) -> Result<TapReader, AttachError> {
        let streams = streams & (AUDIO | LEVELS);
        if streams == 0 {
            return Err(AttachError::NoStreams);
        }
        if self.reader.swap(true, Ordering::AcqRel) {
            return Err(AttachError::Busy);
        }
        if streams & AUDIO != 0 {
            let a = self.audio.get_or_init(AudioRing::new);
            // Start after the last block committed: the reader's indices
            // follow its meta, so a writer publishing now cannot confuse them.
            let mw = a.mw.load(Ordering::Acquire);
            let end = if mw == 0 {
                0
            } else {
                let k = ((mw - 1) as usize % AUDIO_BLOCKS) * META_WORDS;
                a.meta[k].load(Ordering::Relaxed) + (a.meta[k + 2].load(Ordering::Relaxed) & 0xFFFF_FFFF)
            };
            a.r.store(end, Ordering::Release);
            a.mr.store(mw, Ordering::Release);
        }
        if streams & LEVELS != 0 {
            let l = self.levels.get_or_init(LevelRing::new);
            l.r.store(l.w.load(Ordering::Acquire), Ordering::Release);
        }
        self.armed.store(streams, Ordering::Release);
        Ok(TapReader { p: self.clone(), streams })
    }
}

/// The audio thread's hold on a tap point for one `process` call. Dropping
/// it releases the claim.
pub struct TapWriter<'a> {
    p: &'a TapPoint,
    streams: u32,
}

impl Drop for TapWriter<'_> {
    fn drop(&mut self) {
        self.p.claim.store(false, Ordering::Release);
    }
}

/// Space reserved in the audio ring for a block whose pre half is written
/// (see [`TapWriter::audio_pre`]).
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct AudioSlot {
    start: u64,
    frames: usize,
}

impl TapWriter<'_> {
    /// The streams the reader wants.
    pub fn streams(&self) -> u32 {
        self.streams
    }

    /// Reserve room for the block and copy its input (pre). `None`: no
    /// audio stream, or no room (the block is dropped and counted; the next
    /// one that gets through carries the gap flag).
    pub fn audio_pre(&mut self, l: &[f32], r: &[f32]) -> Option<AudioSlot> {
        if self.streams & AUDIO == 0 {
            return None;
        }
        let a = self.p.audio.get()?;
        let n = l.len().min(r.len());
        let w = a.w.load(Ordering::Relaxed);
        let used = w - a.r.load(Ordering::Acquire).min(w);
        let blocks = a.mw.load(Ordering::Relaxed) - a.mr.load(Ordering::Acquire).min(a.mw.load(Ordering::Relaxed));
        if n == 0 || n > AUDIO_FRAMES - used as usize || blocks as usize >= AUDIO_BLOCKS {
            self.p.dropped_blocks.fetch_add(1, Ordering::Relaxed);
            self.p.dropped_frames.fetch_add(n as u64, Ordering::Relaxed);
            a.gap.store(true, Ordering::Relaxed);
            return None;
        }
        for i in 0..n {
            let s = ((w as usize + i) & (AUDIO_FRAMES - 1)) * AUDIO_CHANNELS;
            a.data[s].store(l[i].to_bits(), Ordering::Relaxed);
            a.data[s + 1].store(r[i].to_bits(), Ordering::Relaxed);
        }
        Some(AudioSlot { start: w, frames: n })
    }

    /// Copy the output (post) into the slot and publish the block, with the
    /// song position of its first frame and whether the transport plays.
    pub fn audio_post(&mut self, slot: AudioSlot, l: &[f32], r: &[f32], pos: u64, playing: bool) {
        let Some(a) = self.p.audio.get() else { return };
        let n = slot.frames.min(l.len()).min(r.len());
        for i in 0..n {
            let s = ((slot.start as usize + i) & (AUDIO_FRAMES - 1)) * AUDIO_CHANNELS;
            a.data[s + 2].store(l[i].to_bits(), Ordering::Relaxed);
            a.data[s + 3].store(r[i].to_bits(), Ordering::Relaxed);
        }
        let mw = a.mw.load(Ordering::Relaxed);
        let k = (mw as usize % AUDIO_BLOCKS) * META_WORDS;
        let flags = (if playing { FLAG_PLAYING } else { 0 }) | (if a.gap.swap(false, Ordering::Relaxed) { FLAG_GAP } else { 0 });
        a.meta[k].store(slot.start, Ordering::Relaxed);
        a.meta[k + 1].store(pos, Ordering::Relaxed);
        a.meta[k + 2].store(n as u64 | (flags as u64) << 32, Ordering::Relaxed);
        a.w.store(slot.start + n as u64, Ordering::Relaxed);
        a.mw.store(mw + 1, Ordering::Release);
    }

    /// Publish a level record; dropped and counted if the ring is full.
    pub fn level(&mut self, rec: &LevelRec) {
        if self.streams & LEVELS == 0 {
            return;
        }
        let Some(l) = self.p.levels.get() else { return };
        let w = l.w.load(Ordering::Relaxed);
        if w - l.r.load(Ordering::Acquire).min(w) >= LEVEL_SLOTS as u64 {
            self.p.dropped_levels.fetch_add(1, Ordering::Relaxed);
            l.gap.store(true, Ordering::Relaxed);
            return;
        }
        let k = (w as usize % LEVEL_SLOTS) * LEVEL_WORDS;
        let flags = (if rec.playing { FLAG_PLAYING } else { 0 }) | (if l.gap.swap(false, Ordering::Relaxed) { FLAG_GAP } else { 0 });
        let words = [rec.pos as u32, (rec.pos >> 32) as u32, rec.frames as u32 | flags << 16, rec.in_peak.to_bits(), rec.out_peak.to_bits(), rec.in_sq.to_bits(), rec.out_sq.to_bits(), rec.gr_db.to_bits()];
        for (i, v) in words.into_iter().enumerate() {
            l.words[k + i].store(v, Ordering::Relaxed);
        }
        l.w.store(w + 1, Ordering::Release);
    }
}

/// The reading end of a tap point; dropping it detaches.
pub struct TapReader {
    p: Arc<TapPoint>,
    streams: u32,
}

impl fmt::Debug for TapReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TapReader").field("streams", &self.streams).finish()
    }
}

impl Drop for TapReader {
    fn drop(&mut self) {
        self.p.armed.store(0, Ordering::Release);
        self.p.reader.store(false, Ordering::Release);
    }
}

impl TapReader {
    pub fn streams(&self) -> u32 {
        self.streams
    }

    pub fn stats(&self) -> TapStats {
        self.p.stats()
    }

    /// Append every block written since the last call to `out`.
    pub fn drain_audio(&mut self, out: &mut AudioDrain) {
        let Some(a) = self.p.audio.get().filter(|_| self.streams & AUDIO != 0) else { return };
        let (mr, mw) = (a.mr.load(Ordering::Relaxed), a.mw.load(Ordering::Acquire));
        let mut end = a.r.load(Ordering::Relaxed);
        for m in mr..mw {
            let k = (m as usize % AUDIO_BLOCKS) * META_WORDS;
            let start = a.meta[k].load(Ordering::Relaxed);
            let pos = a.meta[k + 1].load(Ordering::Relaxed);
            let fw = a.meta[k + 2].load(Ordering::Relaxed);
            let (frames, flags) = ((fw & 0xFFFF_FFFF) as usize, (fw >> 32) as u32);
            out.blocks.push(AudioBlock { at: out.pre[0].len(), frames, pos, playing: flags & FLAG_PLAYING != 0, gap: flags & FLAG_GAP != 0 });
            for i in 0..frames {
                let s = ((start as usize + i) & (AUDIO_FRAMES - 1)) * AUDIO_CHANNELS;
                for c in 0..2 {
                    out.pre[c].push(f32::from_bits(a.data[s + c].load(Ordering::Relaxed)));
                    out.post[c].push(f32::from_bits(a.data[s + 2 + c].load(Ordering::Relaxed)));
                }
            }
            end = start + frames as u64;
        }
        a.r.store(end, Ordering::Release);
        a.mr.store(mw, Ordering::Release);
    }

    /// Append every level record written since the last call to `out`.
    pub fn drain_levels(&mut self, out: &mut Vec<LevelRec>) {
        let Some(l) = self.p.levels.get().filter(|_| self.streams & LEVELS != 0) else { return };
        let (r, w) = (l.r.load(Ordering::Relaxed), l.w.load(Ordering::Acquire));
        for i in r..w {
            let k = (i as usize % LEVEL_SLOTS) * LEVEL_WORDS;
            let v = |j: usize| l.words[k + j].load(Ordering::Relaxed);
            let fw = v(2);
            let flags = fw >> 16;
            out.push(LevelRec {
                pos: v(0) as u64 | (v(1) as u64) << 32,
                frames: (fw & 0xFFFF) as u16,
                playing: flags & FLAG_PLAYING != 0,
                gap: flags & FLAG_GAP != 0,
                in_peak: f32::from_bits(v(3)),
                out_peak: f32::from_bits(v(4)),
                in_sq: f32::from_bits(v(5)),
                out_sq: f32::from_bits(v(6)),
                gr_db: f32::from_bits(v(7)),
            });
        }
        l.r.store(w, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(n: usize, base: f32) -> (Vec<f32>, Vec<f32>) {
        ((0..n).map(|i| base + i as f32).collect(), (0..n).map(|i| -base - i as f32).collect())
    }

    fn write(p: &TapPoint, serial: u64, l: &[f32], r: &[f32], pos: u64, playing: bool) -> bool {
        let Some(mut w) = p.writer(serial) else { return false };
        let Some(slot) = w.audio_pre(l, r) else { return false };
        let (pl, pr): (Vec<f32>, Vec<f32>) = (l.iter().map(|v| v * 2.0).collect(), r.iter().map(|v| v * 2.0).collect());
        w.audio_post(slot, &pl, &pr, pos, playing);
        true
    }

    #[test]
    fn an_idle_point_gives_no_writer_and_costs_a_load() {
        let p = TapPoint::new();
        assert_eq!(p.armed(), 0);
        assert!(p.writer(1).is_none());
        assert_eq!(p.stats(), TapStats::default());
        assert!(p.audio.get().is_none() && p.levels.get().is_none(), "no storage until a reader attaches");
    }

    #[test]
    fn blocks_come_out_in_order_with_their_stamps() {
        let p = TapPoint::new();
        let s = p.register();
        let mut rd = p.attach(AUDIO).unwrap();
        let mut at = 1_000u64;
        for k in 0..5 {
            let (l, r) = block(100 + k, 10.0 * k as f32);
            assert!(write(&p, s, &l, &r, at, k != 3));
            at += 100 + k as u64;
        }
        let mut d = AudioDrain::default();
        rd.drain_audio(&mut d);
        assert_eq!(d.blocks.len(), 5);
        assert_eq!(d.frames(), 100 + 101 + 102 + 103 + 104);
        let mut pos = 1_000;
        for (k, b) in d.blocks.iter().enumerate() {
            assert_eq!((b.frames, b.pos, b.playing, b.gap), (100 + k, pos, k != 3, false));
            let (l, r) = block(100 + k, 10.0 * k as f32);
            assert_eq!(&d.pre[0][b.at..b.at + b.frames], &l[..]);
            assert_eq!(&d.pre[1][b.at..b.at + b.frames], &r[..]);
            assert_eq!(d.post[0][b.at], 2.0 * l[0]);
            assert_eq!(d.post[1][b.at + b.frames - 1], 2.0 * r[b.frames - 1]);
            pos += b.frames as u64;
        }
        let mut again = AudioDrain::default();
        rd.drain_audio(&mut again);
        assert!(again.blocks.is_empty(), "taken once");
    }

    #[test]
    fn the_ring_wraps_and_stays_in_order() {
        let p = TapPoint::new();
        let s = p.register();
        let mut rd = p.attach(AUDIO).unwrap();
        let mut d = AudioDrain::default();
        let mut next = 0u64;
        for round in 0..200 {
            let n = 100 + (round * 7) % 400;
            let l: Vec<f32> = (0..n).map(|i| (next + i as u64) as f32).collect();
            assert!(write(&p, s, &l, &l, next, true), "round {round}");
            next += n as u64;
            if round % 3 == 0 {
                rd.drain_audio(&mut d);
            }
        }
        rd.drain_audio(&mut d);
        assert_eq!(p.stats().dropped_blocks, 0);
        assert!(d.pre[0].iter().enumerate().all(|(i, v)| *v == i as f32), "contiguous across the wrap (the ring holds {AUDIO_FRAMES})");
        assert!(next as usize > 3 * AUDIO_FRAMES);
    }

    #[test]
    fn a_full_ring_drops_counts_and_marks_the_gap() {
        let p = TapPoint::new();
        let s = p.register();
        let mut rd = p.attach(AUDIO).unwrap();
        let (l, r) = block(1_024, 1.0);
        let fit = AUDIO_FRAMES / 1_024;
        for k in 0..fit {
            assert!(write(&p, s, &l, &r, k as u64 * 1_024, true));
        }
        assert!(!write(&p, s, &l, &r, 99, true), "full: dropped");
        assert!(!write(&p, s, &l, &r, 99, true));
        assert_eq!(p.stats().dropped_blocks, 2);
        assert_eq!(p.stats().dropped_frames, 2_048);
        let mut d = AudioDrain::default();
        rd.drain_audio(&mut d);
        assert_eq!(d.blocks.len(), fit);
        assert!(d.blocks.iter().all(|b| !b.gap));
        assert!(write(&p, s, &l, &r, 5_000, true));
        rd.drain_audio(&mut d);
        assert!(d.blocks.last().unwrap().gap, "the first block after a drop says so");
        assert!(write(&p, s, &l, &r, 6_000, true));
        rd.drain_audio(&mut d);
        assert!(!d.blocks.last().unwrap().gap);
        assert_eq!(rd.stats().dropped_blocks, 2);
    }

    #[test]
    fn too_many_small_blocks_drop_on_the_block_count() {
        let p = TapPoint::new();
        let s = p.register();
        let _rd = p.attach(AUDIO).unwrap();
        let (l, r) = block(8, 1.0);
        let ok = (0..AUDIO_BLOCKS + 10).filter(|k| write(&p, s, &l, &r, *k as u64, true)).count();
        assert_eq!(ok, AUDIO_BLOCKS);
        assert_eq!(p.stats().dropped_blocks, 10);
    }

    #[test]
    fn levels_round_trip_and_drop_when_full() {
        let p = TapPoint::new();
        let s = p.register();
        let mut rd = p.attach(LEVELS).unwrap();
        let rec = |k: u64| LevelRec { pos: 1 << 33 | k, frames: 32, playing: k.is_multiple_of(2), gap: false, in_peak: k as f32, out_peak: -(k as f32), in_sq: 1.5, out_sq: 2.5, gr_db: 0.25 * k as f32 };
        {
            let mut w = p.writer(s).unwrap();
            for k in 0..LEVEL_SLOTS as u64 + 5 {
                w.level(&rec(k));
            }
        }
        assert_eq!(p.stats().dropped_levels, 5);
        let mut v = Vec::new();
        rd.drain_levels(&mut v);
        assert_eq!(v.len(), LEVEL_SLOTS);
        assert_eq!(v[0], rec(0));
        assert_eq!(v[77], rec(77));
        let mut w = p.writer(s).unwrap();
        w.level(&rec(9_999));
        drop(w);
        rd.drain_levels(&mut v);
        assert!(v.last().unwrap().gap && v.last().unwrap().in_peak == 9_999.0);
    }

    #[test]
    fn one_reader_at_a_time_and_its_drop_disarms() {
        let p = TapPoint::new();
        assert_eq!(p.attach(0).unwrap_err(), AttachError::NoStreams);
        let rd = p.attach(AUDIO | LEVELS).unwrap();
        assert_eq!(p.armed(), AUDIO | LEVELS);
        assert_eq!(p.attach(AUDIO).unwrap_err(), AttachError::Busy);
        drop(rd);
        assert_eq!(p.armed(), 0);
        assert!(p.writer(1).is_none());
        let rd = p.attach(AUDIO).unwrap();
        assert_eq!(p.armed(), AUDIO);
        assert_eq!(rd.streams(), AUDIO);
    }

    #[test]
    fn attaching_again_skips_what_was_left_behind() {
        let p = TapPoint::new();
        let s = p.register();
        let rd = p.attach(AUDIO).unwrap();
        let (l, r) = block(200, 1.0);
        assert!(write(&p, s, &l, &r, 0, true));
        drop(rd);
        let mut rd = p.attach(AUDIO).unwrap();
        let mut d = AudioDrain::default();
        rd.drain_audio(&mut d);
        assert!(d.blocks.is_empty(), "old blocks are not the new reader's");
        assert!(write(&p, s, &l, &r, 200, true));
        rd.drain_audio(&mut d);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].pos, 200);
    }

    #[test]
    fn the_newest_instance_writes_and_an_older_one_is_counted() {
        let p = TapPoint::new();
        let (old, new) = (p.register(), p.register());
        assert!(new > old);
        let mut rd = p.attach(AUDIO).unwrap();
        let (l, r) = block(64, 1.0);
        assert!(write(&p, old, &l, &r, 0, true), "the only instance so far");
        assert!(write(&p, new, &l, &r, 64, true), "the newer takes over");
        assert!(!write(&p, old, &l, &r, 128, true), "the older goes quiet");
        assert!(write(&p, new, &l, &r, 128, true));
        assert_eq!(p.stats().superseded, 1);
        let mut d = AudioDrain::default();
        rd.drain_audio(&mut d);
        assert_eq!(d.blocks.iter().map(|b| b.pos).collect::<Vec<_>>(), vec![0, 64, 128]);
    }

    #[test]
    fn a_writer_in_progress_keeps_a_second_one_out_without_waiting() {
        let p = TapPoint::new();
        let s = p.register();
        let _rd = p.attach(AUDIO).unwrap();
        let first = p.writer(s).unwrap();
        assert!(p.writer(s).is_none());
        assert_eq!(p.stats().contended, 1);
        drop(first);
        assert!(p.writer(s).is_some());
    }

    /// A writer thread and a reader thread: every block arrives once, in
    /// order, with its contents, whatever the interleaving; what does not
    /// fit is counted.
    #[test]
    fn a_writer_and_a_reader_on_two_threads_lose_nothing_unseen() {
        let p = TapPoint::new();
        let s = p.register();
        let mut rd = p.attach(AUDIO | LEVELS).unwrap();
        let blocks = 20_000u64;
        let pw = p.clone();
        let writer = std::thread::spawn(move || {
            for k in 0..blocks {
                let n = 64 + (k % 5) as usize * 32;
                let l: Vec<f32> = (0..n).map(|i| (k * 1_000 + i as u64) as f32).collect();
                if let Some(mut w) = pw.writer(s) {
                    if let Some(slot) = w.audio_pre(&l, &l) {
                        w.audio_post(slot, &l, &l, k, true);
                    }
                    w.level(&LevelRec { pos: k, frames: 32, playing: true, in_peak: k as f32, ..Default::default() });
                }
                if k % 64 == 0 {
                    std::thread::yield_now();
                }
            }
        });
        let (mut seen, mut last) = (0u64, None::<u64>);
        let (mut levels, mut last_level) = (0u64, None::<u64>);
        let mut d = AudioDrain::default();
        let mut v = Vec::new();
        loop {
            d.clear();
            v.clear();
            rd.drain_audio(&mut d);
            rd.drain_levels(&mut v);
            for b in &d.blocks {
                assert!(last.is_none_or(|l| b.pos > l), "positions rise");
                last = Some(b.pos);
                let want = b.pos * 1_000;
                assert!(d.pre[0][b.at..b.at + b.frames].iter().enumerate().all(|(i, x)| *x == (want + i as u64) as f32), "block {} intact", b.pos);
                seen += 1;
            }
            for r in &v {
                assert!(last_level.is_none_or(|l| r.pos > l));
                last_level = Some(r.pos);
                assert_eq!(r.in_peak, r.pos as f32);
                levels += 1;
            }
            if writer.is_finished() {
                break;
            }
        }
        writer.join().unwrap();
        d.clear();
        v.clear();
        rd.drain_audio(&mut d);
        rd.drain_levels(&mut v);
        seen += d.blocks.len() as u64;
        levels += v.len() as u64;
        let st = p.stats();
        assert_eq!(seen + st.dropped_blocks, blocks, "every block seen or counted");
        assert_eq!(levels + st.dropped_levels, blocks, "every record seen or counted");
    }
}
