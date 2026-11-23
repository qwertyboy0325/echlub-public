//! What crosses between the audio thread and the rest of a host through
//! lock-free rings: the recording tap (fixed-size packets of armed tracks'
//! raw input, placed on the song) and live notes (into the engine, and
//! back out placed on the song).
//!
//! Plain data and ring ends only: the engine fills the tap and reads live
//! notes; a host's writer thread or capture buffer reads the tap. All
//! memory is taken in [`rec_tap`]; pushing and popping never allocates.

/// Frames in one recording packet.
pub const PACKET_FRAMES: usize = 128;
/// A piece of one armed track's raw input, placed on the song.
#[derive(Clone, Copy)]
pub struct RecPacket {
    /// Engine track id (the engine's `TrackDesc::id`).
    pub track: u64,
    /// Serial of the straight transport run this belongs to: frames of one
    /// take share it; a loop pass, a new start or a seek gives a new one.
    pub take: u64,
    /// Song frame of the first frame.
    pub pos: u64,
    pub frames: u16,
    pub channels: u8,
    /// Interleaved, `frames * channels` used.
    pub data: [f32; PACKET_FRAMES * 2],
}

impl Default for RecPacket {
    fn default() -> Self {
        RecPacket { track: 0, take: 0, pos: 0, frames: 0, channels: 1, data: [0.0; PACKET_FRAMES * 2] }
    }
}

impl std::fmt::Debug for RecPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecPacket").field("track", &self.track).field("take", &self.take).field("pos", &self.pos).field("frames", &self.frames).field("channels", &self.channels).finish()
    }
}

impl RecPacket {
    pub fn samples(&self) -> &[f32] {
        &self.data[..self.frames as usize * self.channels as usize]
    }
}

/// A note played live, for the engine to sound and to place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveNote {
    /// Track index (as in the engine's `Command::SetMix`) that plays it, or
    /// `None` to only place it (capture with no instrument armed).
    pub track: Option<usize>,
    /// Engine clock frame to sound on (the host's timestamp plus its fixed
    /// scheduling offset).
    pub at: u64,
    /// Engine clock frame whose sound reached the player's ears when the
    /// note was played (the host's timestamp converted through the output
    /// latency): the note belongs where that frame was in the song.
    pub heard: u64,
    pub pitch: u8,
    pub vel: f32,
    pub on: bool,
    /// An audition (a pad or a key pressed in the UI to hear the
    /// instrument): sounded like any live note, never reported back, so it
    /// is not recorded or captured.
    pub audition: bool,
}

/// A live note reported back, placed on the song.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiRec {
    pub track: Option<usize>,
    pub pitch: u8,
    pub vel: f32,
    pub on: bool,
    /// Engine clock of [`LiveNote::heard`].
    pub heard: u64,
    /// The song frame heard when it was played (the transport position
    /// then, when stopped).
    pub pos: u64,
    pub playing: bool,
    /// Recording was on there (and inside the punch range).
    pub recording: bool,
    /// The run serial, as [`RecPacket::take`].
    pub take: u64,
}

/// One straight run of input frames in the last block: frames
/// `offset..offset + len` were played against song frames `pos..` (or, if
/// not `playing`, with the transport stopped at `pos`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputRun {
    pub offset: usize,
    pub len: usize,
    pub pos: u64,
    pub playing: bool,
    pub recording: bool,
    pub take: u64,
}

/// The audio-thread ends of the recording rings (the engine pushes).
pub struct RecTap {
    pub packets: rtrb::Producer<RecPacket>,
    pub notes: rtrb::Producer<MidiRec>,
}

/// The reading ends (a writer thread, the capture buffer).
pub struct RecTapReader {
    pub packets: rtrb::Consumer<RecPacket>,
    pub notes: rtrb::Consumer<MidiRec>,
}

/// Rings for `packet_slots` audio packets (each [`PACKET_FRAMES`] frames of
/// one track) and `note_slots` live notes; all memory taken here.
pub fn rec_tap(packet_slots: usize, note_slots: usize) -> (RecTap, RecTapReader) {
    let (ptx, prx) = rtrb::RingBuffer::new(packet_slots.max(1));
    let (ntx, nrx) = rtrb::RingBuffer::new(note_slots.max(1));
    (RecTap { packets: ptx, notes: ntx }, RecTapReader { packets: prx, notes: nrx })
}

