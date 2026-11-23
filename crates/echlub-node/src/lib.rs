//! The node interface: every sound source and effect in a track's chain
//! is a [`Node`], EchLub's own modules and (later) third-party plugins
//! alike. Shaped after CLAP (and compatible with AU / VST3 adapters):
//!
//! - [`NodeFactory::create`] and [`Node::prepare`] run on the control
//!   thread; all memory is taken there.
//! - [`Node::process`] runs on the audio thread (or an engine worker) once
//!   per block, in place on a stereo buffer (zeros in for a source), with a
//!   sample-accurate, sorted event list for that block: notes, parameter
//!   changes and transport. No polling for parameters.
//! - A node reports its latency (frames from input / event to output) and
//!   its tail; the engine compensates latency across tracks (PDC) and keeps
//!   calling a node after the transport stops until it says it is asleep
//!   and its output is silent.
//! - [`Node::reset`] is the hard cut (the engine's `Command::Panic`); a soft
//!   release is the [`EventKind::AllNotesOff`] event.
//! - State saves and loads as bytes on the control thread.
//! - [`NodeInfo::deterministic`]: the same events give the same output for
//!   any block split, offline or live. The engine's bit-exactness tests
//!   (block size, offline = live, threads) cover only graphs whose nodes
//!   all say so; a future third-party plugin would not.
//!
//! How a future third-party plugin fits (not built; hosting stays a
//! non-goal): an AU or CLAP adapter is one more `NodeFactory` + `Node`
//! that owns the plugin instance, maps `prepare` to activate, `process`
//! to the plugin's process call (converting [`NodeEvent`]s to its event
//! format), `info` to its latency / tail queries, and the state calls to
//! the plugin's state chunk; `deterministic` is false. An out-of-process
//! sandbox is the same shape with `process` writing the block and events
//! into shared memory and waiting, with a deadline, for the helper process
//! to answer; on a miss it outputs silence for the block and reports it.
//! Nothing in the engine changes for either.
//!
//! [`io`] holds the plain types that cross between the audio thread and the
//! rest of a host through lock-free rings (recording packets, live notes in
//! and placed notes out), so the engine that fills those rings and the
//! recording code that empties them both depend on this crate and not on
//! each other.
//!
//! [`tap`] holds the signal tap: a point per insert that an effect node
//! writes its input and output into from the audio thread, and a reader
//! the UI drains.
//!
//! Depends on `std` and `rtrb` (the ring) only; platform-neutral.

use std::any::Any;

pub mod io;
pub mod tap;
pub use rtrb;

/// What a node reports about itself. Fixed from `prepare` on: a node whose
/// latency changes must be rebuilt in a new graph (as CLAP asks for a
/// restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeInfo {
    /// Frames from an input sample or event to its effect at the output.
    pub latency: u32,
    pub tail: Tail,
    /// Output depends only on the events and input, never on the block
    /// split, wall clock, threads or anything outside.
    pub deterministic: bool,
}

/// How long a node may keep sounding after its input and notes stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    None,
    Frames(u32),
    /// Until it reports [`Status::Sleep`] (release envelopes, feedback).
    UntilSilent,
}

/// What `process` says about the node's state after the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Still producing (or about to produce) sound.
    Continue,
    /// Nothing inside is moving: if the output was silent too, the engine
    /// may stop calling `process` until the next event or input.
    Sleep,
}

/// A parameter a node exposes. Values are plain numbers in `min..=max`;
/// the node smooths changes itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    pub id: u32,
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

/// The transport as of an event's frame.
///
/// What a node may assume:
/// - The engine sends one at the start of every block, at every tempo and
///   metre change (on its frame, rounded as the tempo map rounds it), at
///   every start, stop and jump (seek, loop wrap). Within one event and
///   the next, tempo and metre are constant: that stretch lies inside one
///   tempo segment and one metre.
/// - Events reach a node delayed by the latency upstream of it, so they
///   describe the audio it is processing at that frame.
/// - On a jump the new values simply replace the old ones (no
///   continuity).
/// - The musical fields are constants of the segment (the same in every
///   event inside it). A node gets the position of any frame `f` of the
///   segment from them alone, with [`TransportInfo::beat_at`] and
///   [`TransportInfo::bar_at`], computing from the anchor every time and
///   never accumulating: then the result is a function of `f` only, the
///   same bits for any block split, offline or live:
///
///   ```text
///   beat(f)        = segment_beat + (f - segment_frame) / frames_per_beat   (f64)
///   k              = floor((beat - bar_anchor_beat) / bar_len_beats)
///   bar(f)         = bar_anchor_index + k
///   bar_start(f)   = bar_anchor_beat + k * bar_len_beats
///   ```
///   Beats are quarter notes from the song's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportInfo {
    /// Song frame (render position) at this event's frame.
    pub pos: u64,
    pub playing: bool,
    /// The transport jumped here (seek, loop wrap) from this frame;
    /// `u64::MAX` when it did not.
    pub jumped_from: u64,
    /// Tempo, beats per minute.
    pub tempo_bpm: f32,
    /// The tempo segment's start: its exact (fractional) song frame and
    /// its position in quarter notes.
    pub segment_frame: f64,
    pub segment_beat: f64,
    /// Frames per quarter note in this segment (exact, f64).
    pub frames_per_beat: f64,
    /// A bar line of the metre in force (the metre's first bar), in
    /// quarter notes, and that bar's 0-based index.
    pub bar_anchor_beat: f64,
    pub bar_anchor_index: i64,
    /// One bar of that metre, in quarter notes (`4 num / den`).
    pub bar_len_beats: f64,
    pub meter_num: u16,
    pub meter_den: u16,
}

impl TransportInfo {
    pub fn jump(&self) -> Option<u64> {
        (self.jumped_from != u64::MAX).then_some(self.jumped_from)
    }

    /// The song position of frame `f` in quarter notes (see the type's
    /// documentation: from the segment's anchor, never accumulated).
    #[inline]
    pub fn beat_at(&self, f: f64) -> f64 {
        self.segment_beat + (f - self.segment_frame) / self.frames_per_beat
    }

    /// The bar at quarter-note position `beat`: its 0-based index and the
    /// quarter-note position where it starts.
    #[inline]
    pub fn bar_at(&self, beat: f64) -> (i64, f64) {
        let k = ((beat - self.bar_anchor_beat) / self.bar_len_beats).floor();
        (self.bar_anchor_index + k as i64, self.bar_anchor_beat + k * self.bar_len_beats)
    }

    /// A transport at song frame `pos` of a song with one tempo and 4/4
    /// throughout, from frame 0 (for tests and simple hosts).
    pub fn steady(pos: u64, playing: bool, jumped_from: u64, tempo_bpm: f32, sample_rate: u32) -> Self {
        TransportInfo {
            pos,
            playing,
            jumped_from,
            tempo_bpm,
            segment_frame: 0.0,
            segment_beat: 0.0,
            frames_per_beat: sample_rate as f64 * 60.0 / tempo_bpm as f64,
            bar_anchor_beat: 0.0,
            bar_anchor_index: 0,
            bar_len_beats: 4.0,
            meter_num: 4,
            meter_den: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    NoteOn { pitch: u8, vel: f32 },
    NoteOff { pitch: u8 },
    /// Pitch bend from this frame on, -1..1 (the instrument's bend range
    /// says how many semitones a full bend is). Held until the next one.
    PitchBend { value: f32 },
    /// Every held note off: release, do not cut (stop, seek, loop end).
    AllNotesOff,
    /// Set parameter `id` (see [`Node::params`]) from this frame on.
    Param { id: u32, value: f32 },
    /// Move parameter `id` linearly, in its own units, from wherever it is
    /// at this frame to `value` (clamped to its range): the k-th frame from
    /// here (k = 0, 1, ..) has `from + (value - from) (k + 1) / frames`,
    /// so the last frame of the ramp has exactly `value`, which then holds.
    /// No glide of the node's own on top (a node with a control rate
    /// follows the ramp at its control points). `frames` 0: `value` at
    /// once, without a glide (a host sends that only where the sound
    /// jumps anyway: start, seek). A later `Param` or ramp for the same id
    /// replaces it from its frame. For a parameter that does not glide (a
    /// choice, a switch) and for a node that does not implement ramps, a
    /// ramp is `Param { id, value }` at its frame. Used by automation.
    ParamRamp { id: u32, value: f32, frames: u32 },
    Transport(TransportInfo),
}

/// One event at `frame` (0-based, inside the block being processed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeEvent {
    pub frame: u32,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// This node has no saveable state format yet.
    Unsupported,
    Invalid(String),
}

/// A sound source or effect. See the module docs for the contract.
pub trait Node: Send {
    fn info(&self) -> NodeInfo;
    fn params(&self) -> &[ParamInfo] {
        &[]
    }
    /// Control thread, before the node goes to the audio thread: take all
    /// memory for blocks up to `max_block` frames here.
    fn prepare(&mut self, sample_rate: u32, max_block: usize);
    /// Audio thread. `left` and `right` (same length, at most `max_block`)
    /// hold the input (zeros for a source) and receive the output.
    /// `events` are sorted by frame, every frame below `left.len()`. Must
    /// not allocate, free, lock or block. May be slow (a future plugin):
    /// the engine does not assume a bound.
    fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[NodeEvent]) -> Status;
    /// The node listens to a key (sidechain) input: a host gives it one
    /// through [`Node::process_keyed`]. Fixed for the node's life.
    fn key_input(&self) -> bool {
        false
    }
    /// Audio thread: [`Node::process`] with a key (sidechain) signal,
    /// `key_left` / `key_right` as long as `left`, frame for frame (the
    /// host aligns it to the audio). A node without a key input ignores it
    /// (the default); a keyed node given only `process` hears a silent key.
    fn process_keyed(&mut self, left: &mut [f32], right: &mut [f32], key_left: &[f32], key_right: &[f32], events: &[NodeEvent]) -> Status {
        let _ = (key_left, key_right);
        self.process(left, right, events)
    }
    /// Audio thread: silence at once, all sound state cleared.
    fn reset(&mut self);
    /// Control thread.
    fn save_state(&self) -> Result<Vec<u8>, StateError> {
        Err(StateError::Unsupported)
    }
    /// Control thread (before the node is sent to the audio thread).
    fn load_state(&mut self, _bytes: &[u8]) -> Result<(), StateError> {
        Err(StateError::Unsupported)
    }
    /// Audio thread, at a graph swap: continue from `old` (a node of the
    /// track this one replaces) at the transport `t`, without a break.
    /// Return false if it cannot; the engine then lets `old` ring out and
    /// starts this one fresh. Must not allocate or free.
    fn take_over(&mut self, _old: &mut dyn Node, _t: &TransportInfo) -> bool {
        false
    }
    /// Still has sound to give after being replaced (fade-outs, tails).
    fn busy(&self) -> bool {
        false
    }
    /// Audio thread, at a graph swap: this instance has been replaced in
    /// the playing graph by another one (the instance that moved into the
    /// new graph, or the one that took over from it) and is left in the old
    /// graph, which only rings out. A node that publishes to an observer
    /// (the effects' signal tap) stops. Called once; must not allocate or
    /// free. Changes nothing a node plays.
    fn retired(&mut self) {}
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Makes a node (control thread). Graph descriptions hold factories so
/// they stay cheap to clone and compare.
pub trait NodeFactory: Send + Sync + std::fmt::Debug {
    fn create(&self) -> Box<dyn Node>;
    /// Same kind and settings: at a graph swap the old node instance is
    /// simply moved into the new graph (voices and state carry over).
    fn same(&self, other: &dyn NodeFactory) -> bool;
    /// Not the same settings, but a node from this factory can continue
    /// one from `other` without a break ([`Node::take_over`]): at a graph
    /// swap it does, instead of starting fresh.
    fn continues(&self, _other: &dyn NodeFactory) -> bool {
        false
    }
    fn as_any(&self) -> &dyn Any;
}
