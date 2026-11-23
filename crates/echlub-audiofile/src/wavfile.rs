//! A crash-safe streaming WAV writer for recordings (32-bit float) and the
//! repair step for a file whose writer died.
//!
//! Layout: `RIFF` + `fmt ` (IEEE float, 18 bytes) + `fact` + `data`; the
//! samples start at byte [`DATA_AT`]. The sizes in the header are rewritten
//! at every [`WavWriter::checkpoint`] (and `fsync`ed), so after a crash the
//! file is a valid WAV of everything up to the last checkpoint, and
//! [`repair`] recovers the rest that reached the disk (whole frames only).
//!
//! Limit: plain RIFF, so one file holds at most 4 GiB of samples (about 3.1
//! hours of 48 kHz stereo float); the writer refuses to go past that rather
//! than writing a broken header (RF64 is not implemented).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Byte offset of the first sample.
pub const DATA_AT: u64 = 12 + (8 + 18) + (8 + 4) + 8;
const FLOAT: u16 = 3;
const MAX_DATA: u64 = u32::MAX as u64 - DATA_AT;

/// A channel count and rate the header can describe: at least one channel,
/// and a block align (4 bytes per channel) and a byte rate that fit their
/// 16- and 32-bit fields.
fn layout_fits(channels: u16, rate: u32) -> bool {
    channels > 0 && channels <= u16::MAX / 4 && rate as u64 * channels as u64 * 4 <= u32::MAX as u64
}

pub struct WavWriter {
    out: BufWriter<File>,
    path: PathBuf,
    channels: u16,
    rate: u32,
    frames: u64,
}

fn header(channels: u16, rate: u32, frames: u64) -> Vec<u8> {
    let data = frames * channels as u64 * 4;
    let mut b = Vec::with_capacity(DATA_AT as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&((DATA_AT - 8 + data) as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&18u32.to_le_bytes());
    b.extend_from_slice(&FLOAT.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * channels as u32 * 4).to_le_bytes());
    b.extend_from_slice(&(channels * 4).to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    b.extend_from_slice(b"fact");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&(frames as u32).to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data as u32).to_le_bytes());
    debug_assert_eq!(b.len() as u64, DATA_AT);
    b
}

impl WavWriter {
    /// Create `path` (it must not exist) with an empty data chunk.
    pub fn create(path: &Path, channels: u16, rate: u32) -> io::Result<Self> {
        if !layout_fits(channels, rate) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{channels} channels at {rate} Hz cannot be written as a WAV header")));
        }
        let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
        f.write_all(&header(channels, rate, 0))?;
        f.sync_all()?;
        Ok(WavWriter { out: BufWriter::with_capacity(1 << 18, f), path: path.to_path_buf(), channels, rate, frames: 0 })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn frames(&self) -> u64 {
        self.frames
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Append interleaved samples (whole frames).
    pub fn append(&mut self, samples: &[f32]) -> io::Result<()> {
        let ch = self.channels as usize;
        let frames = (samples.len() / ch) as u64;
        if (self.frames + frames) * ch as u64 * 4 > MAX_DATA {
            return Err(io::Error::other("recording longer than a RIFF WAV can hold (4 GiB)"));
        }
        for s in &samples[..frames as usize * ch] {
            self.out.write_all(&s.to_le_bytes())?;
        }
        self.frames += frames;
        Ok(())
    }

    /// Append `frames` frames of silence (a gap the engine reported lost).
    pub fn append_silence(&mut self, frames: u64) -> io::Result<()> {
        let zeros = vec![0.0f32; 256.max(self.channels as usize)];
        let mut left = frames * self.channels as u64;
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            let n = n - n % self.channels as usize;
            let n = if n == 0 { self.channels as usize } else { n };
            self.append(&zeros[..n])?;
            left -= n as u64;
        }
        Ok(())
    }

    /// Flush, rewrite the header sizes, and make both durable.
    pub fn checkpoint(&mut self) -> io::Result<()> {
        self.out.flush()?;
        let f = self.out.get_mut();
        f.sync_data()?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(self.channels, self.rate, self.frames))?;
        f.seek(SeekFrom::End(0))?;
        f.sync_data()
    }

    /// Final header; returns the frames written.
    pub fn finish(mut self) -> io::Result<u64> {
        self.checkpoint()?;
        Ok(self.frames)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    /// Frames the header claimed.
    pub header_frames: u64,
    /// Whole frames on disk (what the file now says).
    pub frames: u64,
    /// Bytes of a torn last frame cut off.
    pub cut_bytes: u64,
}

/// Fix the header of a recording whose writer died: count the whole frames
/// that reached the disk, cut a torn last frame, rewrite the sizes.
pub fn repair(path: &Path) -> Result<Repair, String> {
    let err = |e: io::Error| format!("{}: {e}", path.display());
    let mut f = OpenOptions::new().read(true).write(true).open(path).map_err(err)?;
    let mut h = vec![0u8; DATA_AT as usize];
    f.read_exact(&mut h).map_err(|_| format!("{}: shorter than a WAV header", path.display()))?;
    let u16_at = |i: usize| u16::from_le_bytes([h[i], h[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    if &h[0..4] != b"RIFF" || &h[8..16] != b"WAVEfmt " || &h[38..42] != b"fact" || &h[50..54] != b"data" || u16_at(20) != FLOAT {
        return Err(format!("{}: not a recording written by EchLub", path.display()));
    }
    let channels = u16_at(22);
    let rate = u32_at(24);
    if !layout_fits(channels, rate) {
        return Err(format!("{}: not a recording written by EchLub (header says {channels} channels at {rate} Hz)", path.display()));
    }
    let align = channels as u64 * 4;
    let header_frames = u32_at(54) as u64 / align;
    let len = f.metadata().map_err(err)?.len();
    let body = len.saturating_sub(DATA_AT);
    if body > MAX_DATA {
        return Err(format!("{}: longer than a RIFF WAV can hold (4 GiB); not repaired", path.display()));
    }
    let frames = body / align;
    let cut_bytes = body - frames * align;
    if cut_bytes > 0 {
        f.set_len(DATA_AT + frames * align).map_err(err)?;
    }
    f.seek(SeekFrom::Start(0)).map_err(err)?;
    f.write_all(&header(channels, rate, frames)).map_err(err)?;
    f.sync_all().map_err(err)?;
    Ok(Repair { header_frames, frames, cut_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav;

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("echlub-wavfile-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_finished_recording_reads_back_exactly() {
        let p = temp("ok.wav");
        let x: Vec<f32> = (0..9_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut w = WavWriter::create(&p, 2, 48_000).unwrap();
        w.append(&x[..4_000]).unwrap();
        w.checkpoint().unwrap();
        w.append(&x[4_000..]).unwrap();
        w.append_silence(10).unwrap();
        assert_eq!(w.finish().unwrap(), 4_510);
        let (d, rate) = wav::decode(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!((d.channels, rate, d.frames()), (2, 48_000, 4_510));
        echlub_rt_testkit::bits::assert_same_bits(&d.samples[..9_000], &x, "read back against written");
        assert!(d.samples[9_000..].iter().all(|v| *v == 0.0));
        let _ = std::fs::remove_file(p);
    }

    /// A writer that dies (no final header) after a checkpoint and more
    /// writes, the last frame torn in half: the header says the
    /// checkpoint's length; repair recovers every whole frame on disk.
    #[test]
    fn a_half_written_recording_is_repaired_to_every_whole_frame() {
        let p = temp("crash.wav");
        let x: Vec<f32> = (0..20_000).map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0).collect();
        {
            let mut w = WavWriter::create(&p, 2, 44_100).unwrap();
            w.append(&x[..6_000]).unwrap();
            w.checkpoint().unwrap();
            w.append(&x[6_000..]).unwrap();
            w.out.flush().unwrap();
            // The process dies here: no finish, no header update.
            std::mem::forget(w);
        }
        // And the last write only half reached the disk.
        let len = std::fs::metadata(&p).unwrap().len();
        OpenOptions::new().write(true).open(&p).unwrap().set_len(len - 6).unwrap();
        let before = wav::decode(&std::fs::read(&p).unwrap()).unwrap().0;
        assert_eq!(before.frames(), 3_000, "the header still says the checkpoint");
        let r = repair(&p).unwrap();
        assert_eq!(r, Repair { header_frames: 3_000, frames: 9_999, cut_bytes: 2 });
        let (d, rate) = wav::decode(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!((d.channels, rate, d.frames()), (2, 44_100, 9_999));
        echlub_rt_testkit::bits::assert_same_bits(&d.samples, &x[..19_998], "repaired against written");
        // Repairing a sound file again changes nothing.
        assert_eq!(repair(&p).unwrap(), Repair { header_frames: 9_999, frames: 9_999, cut_bytes: 0 });
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn repair_refuses_other_files() {
        let p = temp("pcm.wav");
        std::fs::write(&p, wav::encode(&[0.0; 200], 2, 48_000, false)).unwrap();
        assert!(repair(&p).unwrap_err().contains("not a recording"));
        let _ = std::fs::remove_file(p);
    }

    /// A recording of `frames` frames, finished, as bytes.
    fn recording(name: &str, channels: u16, rate: u32, frames: usize) -> Vec<u8> {
        let p = temp(name);
        let x: Vec<f32> = (0..frames * channels as usize).map(|i| (i as f32 * 0.3).sin()).collect();
        let mut w = WavWriter::create(&p, channels, rate).unwrap();
        w.append(&x).unwrap();
        w.finish().unwrap();
        let b = std::fs::read(&p).unwrap();
        let _ = std::fs::remove_file(p);
        b
    }

    /// Every prefix of a valid recording (a writer that died after any byte)
    /// is repaired to its whole frames or refused: never a panic, and what is
    /// left is a valid WAV of exactly those frames; repairing again changes
    /// nothing.
    #[test]
    fn every_prefix_of_a_recording_is_repaired_or_refused() {
        let full = recording("prefix-src.wav", 2, 48_000, 5);
        let p = temp("prefix.wav");
        for n in 0..=full.len() {
            std::fs::write(&p, &full[..n]).unwrap();
            match repair(&p) {
                Err(_) => assert!(n < DATA_AT as usize, "prefix {n} of {} was refused", full.len()),
                Ok(r) => {
                    assert!(n >= DATA_AT as usize, "prefix {n} is shorter than a header");
                    let whole = (n - DATA_AT as usize) / 8;
                    assert_eq!((r.frames, r.cut_bytes), (whole as u64, ((n - DATA_AT as usize) % 8) as u64), "prefix {n}");
                    let (d, rate) = wav::decode(&std::fs::read(&p).unwrap()).unwrap();
                    assert_eq!((d.channels, rate, d.frames()), (2, 48_000, whole), "prefix {n}");
                    assert_eq!(repair(&p).unwrap().cut_bytes, 0, "repairing twice, prefix {n}");
                }
            }
        }
        let _ = std::fs::remove_file(p);
    }

    /// Headers that lie: the sizes and the format fields are rewritten by
    /// `repair` from the file's real length, and nothing here may panic or
    /// overflow (these also run in debug builds).
    #[test]
    fn hostile_recording_headers_are_refused_or_repaired_without_panicking() {
        let good = recording("hostile-src.wav", 2, 48_000, 4);
        let p = temp("hostile.wav");
        let run = |what: &str, edit: &dyn Fn(&mut Vec<u8>)| {
            let mut b = good.clone();
            edit(&mut b);
            std::fs::write(&p, &b).unwrap();
            let r = repair(&p);
            if r.is_ok() {
                // Whatever it accepted, it left a file that decodes.
                assert!(wav::decode(&std::fs::read(&p).unwrap()).is_ok(), "{what}: repaired file does not decode");
            }
            r
        };
        // A huge declared data length and frame count: only the real length counts.
        let r = run("huge sizes", &|b| {
            b[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
            b[54..58].copy_from_slice(&u32::MAX.to_le_bytes());
        })
        .unwrap();
        assert_eq!(r.frames, 4);
        // Zero channels: refused (it is not a recording this writer made).
        assert!(run("zero channels", &|b| b[22..24].copy_from_slice(&0u16.to_le_bytes())).is_err());
        // A sample rate and channel count whose byte rate does not fit 32 bits.
        assert!(run("byte rate overflow", &|b| {
            b[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        })
        .is_err());
        assert!(run("channel count overflow", &|b| b[22..24].copy_from_slice(&u16::MAX.to_le_bytes())).is_err());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_writer_refuses_a_layout_it_cannot_write_and_handles_many_channels() {
        let p = temp("layout.wav");
        assert!(WavWriter::create(&p, 0, 48_000).is_err(), "zero channels");
        assert!(!p.exists(), "nothing is created for a refused layout");
        assert!(WavWriter::create(&p, 2, u32::MAX).is_err(), "byte rate beyond 32 bits");
        assert!(!p.exists());
        // More channels than the silence buffer is long.
        let mut w = WavWriter::create(&p, 300, 48_000).unwrap();
        w.append_silence(3).unwrap();
        assert_eq!(w.finish().unwrap(), 3);
        let (d, _) = wav::decode(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!((d.channels, d.frames()), (2, 3), "300 channels fold to stereo");
        let _ = std::fs::remove_file(p);
    }
}
