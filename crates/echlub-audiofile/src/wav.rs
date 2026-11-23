//! WAV in and out for the engine's hosts: 16/24/32-bit PCM and 32-bit
//! float read, 16-bit PCM or 32-bit float written. Allocates; not for the
//! audio thread.

/// Decoded audio: interleaved samples, `channels` 1 or 2 (plain buffers;
/// a host turns them into whatever its engine plays).
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// Reads one sample from its bytes.
type SampleReader = Box<dyn Fn(&[u8]) -> f32>;

#[derive(Debug, PartialEq, Eq)]
pub enum WavError {
    NotWav,
    Unsupported(String),
    Truncated,
}

/// Decoded WAV: the audio (1 or 2 channels; more are folded to stereo) and
/// its sample rate.
pub fn decode(bytes: &[u8]) -> Result<(Decoded, u32), WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32_at(i + 4) as usize;
        let body = i + 8;
        // `None`: the declared end does not fit in a `usize` (a 32-bit target):
        // longer than any file.
        let end = body.checked_add(len);
        if id != b"data" && end.is_none_or(|e| e > bytes.len()) {
            return Err(WavError::Truncated);
        }
        if id == b"fmt " {
            if len < 16 {
                return Err(WavError::Truncated);
            }
            let mut format = u16_at(body);
            if format == 0xFFFE && len >= 26 {
                format = u16_at(body + 24); // WAVE_FORMAT_EXTENSIBLE: the sub-format
            }
            fmt = Some((format, u16_at(body + 2), u32_at(body + 4), u16_at(body + 14)));
        } else if id == b"data" {
            let (format, channels, rate, bits) = fmt.ok_or(WavError::Unsupported("data before fmt".into()))?;
            if channels == 0 {
                return Err(WavError::Unsupported("0 channels".into()));
            }
            let data = &bytes[body..end.map_or(bytes.len(), |e| e.min(bytes.len()))];
            let ch = channels as usize;
            let read: SampleReader = match (format, bits) {
                (1, 16) => Box::new(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
                (1, 24) => Box::new(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0),
                (1, 32) => Box::new(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0),
                (3, 32) => Box::new(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                other => return Err(WavError::Unsupported(format!("format {} at {} bits", other.0, other.1))),
            };
            let step = bits as usize / 8;
            let frames = data.len() / (step * ch);
            let out_ch = ch.min(2);
            let mut samples = Vec::with_capacity(frames * out_ch);
            for f in 0..frames {
                let at = |c: usize| read(&data[(f * ch + c) * step..]);
                if ch <= 2 {
                    for c in 0..ch {
                        samples.push(at(c));
                    }
                } else {
                    // Fold channels 0, 2, ... into the left, 1, 3, ... into the right.
                    let (mut l, mut r) = (0.0, 0.0);
                    for c in 0..ch {
                        if c % 2 == 0 { l += at(c) } else { r += at(c) }
                    }
                    let k = (ch / 2) as f32;
                    samples.push(l / k);
                    samples.push(r / k);
                }
            }
            return Ok((Decoded { channels: out_ch as u16, samples }, rate));
        }
        match end.and_then(|e| e.checked_add(len & 1)) {
            Some(next) => i = next,
            None => break,
        }
    }
    Err(WavError::Unsupported("no data chunk".into()))
}

/// A WAV's `smpl` chunk (sampler data): the note the file plays at its
/// own pitch and its loops, (start, end) in frames, `end` the last frame of
/// the loop (inclusive, as the chunk stores it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmplInfo {
    pub unity_note: u8,
    pub loops: Vec<(u32, u32)>,
}

/// The `smpl` chunk of a WAV, if it has one (wherever it sits: often after
/// the audio). Malformed chunks read as none.
pub fn smpl(bytes: &[u8]) -> Option<SmplInfo> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32_at(i + 4)? as usize;
        let body = i + 8;
        if id == b"smpl" {
            if len < 36 || body + len > bytes.len() {
                return None;
            }
            let unity = u32_at(body + 12)?.min(127) as u8;
            let n = u32_at(body + 28)? as usize;
            let mut loops = Vec::new();
            for k in 0..n {
                let at = body.saturating_add(36).saturating_add(k.saturating_mul(24));
                if at.saturating_add(24) > body + len {
                    break;
                }
                let (start, end) = (u32_at(at + 8)?, u32_at(at + 12)?);
                if end > start {
                    loops.push((start, end));
                }
            }
            return Some(SmplInfo {
                unity_note: unity,
                loops,
            });
        }
        i = body.checked_add(len)?.checked_add(len & 1)?;
    }
    None
}

/// `wav` with a `smpl` chunk appended (for fixtures and tests).
pub fn with_smpl(mut wav: Vec<u8>, info: &SmplInfo) -> Vec<u8> {
    let mut c = Vec::new();
    for v in [
        0u32,
        0,
        0,
        info.unity_note as u32,
        0,
        0,
        0,
        info.loops.len() as u32,
        0,
    ] {
        c.extend_from_slice(&v.to_le_bytes());
    }
    for (k, &(a, b)) in info.loops.iter().enumerate() {
        for v in [k as u32, 0, a, b, 0, 0] {
            c.extend_from_slice(&v.to_le_bytes());
        }
    }
    wav.extend_from_slice(b"smpl");
    wav.extend_from_slice(&(c.len() as u32).to_le_bytes());
    wav.extend_from_slice(&c);
    let riff = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&riff.to_le_bytes());
    wav
}

/// Interleaved samples as a WAV: 32-bit float when `float`, else 16-bit PCM.
pub fn encode(samples: &[f32], channels: u16, sample_rate: u32, float: bool) -> Vec<u8> {
    let bytes_per = if float { 4 } else { 2 };
    let data_len = samples.len() * bytes_per;
    let mut b = Vec::with_capacity(44 + data_len);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&(if float { 3u16 } else { 1 }).to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&sample_rate.to_le_bytes());
    b.extend_from_slice(&(sample_rate * channels as u32 * bytes_per as u32).to_le_bytes());
    b.extend_from_slice(&(channels * bytes_per as u16).to_le_bytes());
    b.extend_from_slice(&(bytes_per as u16 * 8).to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        if float {
            b.extend_from_slice(&s.to_le_bytes());
        } else {
            b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
    }
    b
}

/// The seed of the dither's random stream: fixed, so that the same samples
/// give the same file bytes (renders are deterministic; so are exports).
pub const DITHER_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// TPDF dither: the sum of two independent uniform values of one LSB each
/// less one another, i.e. a triangular distribution on (-1, +1) LSB (2 LSB
/// peak to peak), mean 0, variance 1/6 LSB^2 (RMS 0.408 LSB). Added to the
/// sample, in LSBs of the target word length, just before rounding to it:
/// the rounding error is then independent of the signal (no distortion from
/// the quantisation, only a steady noise: total error RMS 0.5 LSB, against
/// 0.289 LSB for plain rounding). The generator is SplitMix64 from a fixed
/// seed ([`DITHER_SEED`]).
#[derive(Debug, Clone)]
pub struct Tpdf {
    state: u64,
}

impl Tpdf {
    pub const fn new(seed: u64) -> Tpdf {
        Tpdf { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The next dither value in LSBs, in (-1, 1).
    pub fn sample(&mut self) -> f64 {
        let r = self.next_u64();
        // Two 32-bit halves, each a uniform value in [0, 1).
        let (a, b) = ((r >> 32) as f64 / 4_294_967_296.0, (r & 0xFFFF_FFFF) as f64 / 4_294_967_296.0);
        a - b
    }
}

/// Interleaved samples as a 16- or 24-bit PCM WAV (`bits` 16 or 24), and the
/// count of samples beyond full scale (or NaN) that were clamped. A sample
/// is clamped to full scale, scaled by `2^(bits-1) - 1` (the conversion of
/// [`encode`] at 16 bits and of the FLAC writer), and rounded; with `dither`
/// TPDF dither ([`Tpdf`], from [`DITHER_SEED`]) is added just before the
/// rounding, one value per sample. Dither belongs here and nowhere else:
/// reducing the word length is the only thing that needs it (a 32-bit float
/// file is never touched).
pub fn encode_pcm(samples: &[f32], channels: u16, sample_rate: u32, bits: u8, dither: bool) -> (Vec<u8>, usize) {
    let bits = if bits >= 24 { 24u8 } else { 16 };
    let bytes_per = bits as usize / 8;
    let scale = if bits == 16 { 32767.0f64 } else { 8_388_607.0 };
    let data_len = samples.len() * bytes_per;
    let mut b = Vec::with_capacity(44 + data_len);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&sample_rate.to_le_bytes());
    b.extend_from_slice(&(sample_rate * channels as u32 * bytes_per as u32).to_le_bytes());
    b.extend_from_slice(&(channels * bytes_per as u16).to_le_bytes());
    b.extend_from_slice(&(bits as u16).to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    let mut noise = Tpdf::new(DITHER_SEED);
    let mut clipped = 0usize;
    for &s in samples {
        if s.abs() > 1.0 || s.is_nan() {
            clipped += 1;
        }
        let x = if s.is_nan() { 0.0 } else { (s as f64).clamp(-1.0, 1.0) * scale };
        let q = if dither { x + noise.sample() } else { x };
        let v = (q.round() as i32).clamp(-(scale as i32), scale as i32);
        b.extend_from_slice(&v.to_le_bytes()[..bytes_per]);
    }
    (b, clipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smpl_loops_are_read_wherever_the_chunk_is() {
        let x: Vec<f32> = (0..1_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let info = SmplInfo {
            unity_note: 57,
            loops: vec![(200, 799)],
        };
        let wav = with_smpl(encode(&x, 1, 48_000, false), &info);
        assert_eq!(smpl(&wav), Some(info));
        let (d, _) = decode(&wav).unwrap();
        assert_eq!(d.frames(), 1_000, "the audio unchanged");
        assert_eq!(smpl(&encode(&x, 1, 48_000, false)), None);
        assert_eq!(
            smpl(b"RIFF\x10\0\0\0WAVEsmpl\xff\xff\xff\x7f"),
            None,
            "a malformed chunk"
        );
    }

    #[test]
    fn float_and_pcm_round_trip() {
        let x: Vec<f32> = (0..200).map(|i| ((i as f32) * 0.07).sin() * 0.8).collect();
        let (a, sr) = decode(&encode(&x, 2, 44_100, true)).unwrap();
        assert_eq!((a.channels, sr), (2, 44_100));
        echlub_rt_testkit::bits::assert_same_bits(&a.samples, &x, "float round trip");
        let (p, _) = decode(&encode(&x, 1, 48_000, false)).unwrap();
        assert_eq!(p.channels, 1);
        assert!(p.samples.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1.0 / 16_000.0));
    }

    /// A PCM WAV of `bits` per sample from raw little-endian sample bytes.
    fn pcm(bits: u16, ch: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut b = encode(&[], ch, rate, false);
        // Patch the header written for 16-bit: bits, block align, byte rate, sizes.
        let bpf = ch * bits / 8;
        b[28..32].copy_from_slice(&(rate * bpf as u32).to_le_bytes());
        b[32..34].copy_from_slice(&bpf.to_le_bytes());
        b[34..36].copy_from_slice(&bits.to_le_bytes());
        b[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        let riff = b.len() as u32 - 8;
        b[4..8].copy_from_slice(&riff.to_le_bytes());
        b
    }

    #[test]
    fn pcm_24_and_32_bit() {
        // 24-bit stereo: 0.5, -0.25.
        let (d, r) = decode(&pcm(24, 2, 96_000, &[0x00, 0x00, 0x40, 0x00, 0x00, 0xE0])).unwrap();
        assert_eq!((d.channels, r, d.samples), (2, 96_000, vec![0.5, -0.25]));
        // 32-bit mono: -1.0, 0.5.
        let mut data = i32::MIN.to_le_bytes().to_vec();
        data.extend_from_slice(&0x4000_0000i32.to_le_bytes());
        let (d, _) = decode(&pcm(32, 1, 22_050, &data)).unwrap();
        assert_eq!(d.samples, vec![-1.0, 0.5]);
        // 8-bit is refused, not misread.
        assert!(matches!(decode(&pcm(8, 1, 8_000, &[0x80])), Err(WavError::Unsupported(_))));
    }

    fn music(n: usize) -> Vec<f32> {
        // Two sines that never repeat within the test, at about -6 dBFS.
        (0..n).map(|i| (0.35 * (i as f64 * 0.131_7).sin() + 0.15 * (i as f64 * 0.0719).sin()) as f32).collect()
    }

    #[test]
    fn pcm_headers_say_the_bit_depth() {
        let x = music(2_000);
        for (bits, bytes) in [(16u8, 2usize), (24, 3)] {
            let (w, clipped) = encode_pcm(&x, 2, 44_100, bits, true);
            assert_eq!(clipped, 0);
            assert_eq!(&w[0..4], b"RIFF");
            assert_eq!(u16::from_le_bytes([w[20], w[21]]), 1, "PCM");
            assert_eq!(u16::from_le_bytes([w[22], w[23]]), 2, "channels");
            assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 44_100);
            assert_eq!(u32::from_le_bytes([w[28], w[29], w[30], w[31]]), 44_100 * 2 * bytes as u32, "byte rate");
            assert_eq!(u16::from_le_bytes([w[32], w[33]]), 2 * bytes as u16, "block align");
            assert_eq!(u16::from_le_bytes([w[34], w[35]]) as usize, bits as usize, "bits per sample");
            assert_eq!(u32::from_le_bytes([w[40], w[41], w[42], w[43]]) as usize, x.len() * bytes, "data length");
            assert_eq!(w.len(), 44 + x.len() * bytes);
            let (d, rate) = decode(&w).unwrap();
            assert_eq!((d.channels, rate, d.samples.len()), (2, 44_100, x.len()));
        }
    }

    /// Error of the file against the float, in LSBs of the word length
    /// (the integers as written against the float times `2^(bits-1) - 1`).
    fn error_lsb(x: &[f32], w: &[u8], bits: u8) -> Vec<f64> {
        let scale = if bits == 16 { 32767.0 } else { 8_388_607.0 };
        let bytes = bits as usize / 8;
        w[44..]
            .chunks_exact(bytes)
            .zip(x)
            .map(|(c, f)| {
                let raw = if bytes == 2 { i16::from_le_bytes([c[0], c[1]]) as i32 } else { i32::from_le_bytes([c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xFF } else { 0 }]) };
                raw as f64 - *f as f64 * scale
            })
            .collect()
    }

    fn rms(e: &[f64]) -> f64 {
        (e.iter().map(|v| v * v).sum::<f64>() / e.len() as f64).sqrt()
    }

    /// Plain rounding errs by 1/sqrt(12) = 0.289 LSB RMS; TPDF dither adds
    /// noise of variance 1/6 LSB^2 (peak +-1 LSB) to that: sqrt(1/12 + 1/6)
    /// = 0.5 LSB RMS, zero mean, and never more than 1.5 LSB.
    #[test]
    fn tpdf_dither_has_the_stated_level() {
        let x = music(480_000);
        for bits in [16u8, 24] {
            let plain = error_lsb(&x, &encode_pcm(&x, 2, 48_000, bits, false).0, bits);
            let dith = error_lsb(&x, &encode_pcm(&x, 2, 48_000, bits, true).0, bits);
            let (rp, rd) = (rms(&plain), rms(&dith));
            assert!((rp - 0.2887).abs() < 0.005, "{bits}-bit rounding error {rp} LSB RMS");
            assert!((rd - 0.5).abs() < 0.005, "{bits}-bit dithered error {rd} LSB RMS");
            let mean = dith.iter().sum::<f64>() / dith.len() as f64;
            assert!(mean.abs() < 0.005, "{bits}-bit dither mean {mean}");
            let peak = dith.iter().fold(0.0f64, |a, v| a.max(v.abs()));
            assert!(peak <= 1.5 + 1e-3 && peak > 1.3, "{bits}-bit dither peak error {peak} LSB (1 of dither + 0.5 of rounding)");
            // The dither alone, as the difference of the two files: RMS 0.408.
            let noise: Vec<f64> = dith.iter().zip(&plain).map(|(a, b)| a - b).collect();
            // (the rounding differs where the dither tips it, so this is the
            // dither seen through the rounding: still between 0.3 and 0.6)
            assert!((0.3..0.6).contains(&rms(&noise)), "{}", rms(&noise));
        }
        // The generator's own values: triangular, variance 1/6, within (-1, 1).
        let mut g = Tpdf::new(DITHER_SEED);
        let v: Vec<f64> = (0..400_000).map(|_| g.sample()).collect();
        assert!(v.iter().all(|d| d.abs() < 1.0));
        assert!((rms(&v) - (1.0f64 / 6.0).sqrt()).abs() < 0.003, "{}", rms(&v));
        // Triangular, not uniform: a quarter of the values lie beyond half an LSB (P(|d| > 0.5) = 1/4).
        let outer = v.iter().filter(|d| d.abs() > 0.5).count() as f64 / v.len() as f64;
        assert!((outer - 0.25).abs() < 0.005, "{outer}");
    }

    /// The same samples give the same bytes; dither never changes which
    /// samples are clipped; 32-bit float is not touched by any of it.
    #[test]
    fn dither_is_deterministic_counts_clips_and_leaves_float_alone() {
        let mut x = music(5_000);
        x[10] = 1.7;
        x[11] = -3.0;
        x[12] = f32::NAN;
        let a = encode_pcm(&x, 2, 48_000, 16, true);
        assert_eq!(a, encode_pcm(&x, 2, 48_000, 16, true));
        assert_eq!(a.1, 3);
        assert_eq!(encode_pcm(&x, 2, 48_000, 16, false).1, 3);
        let (d, _) = decode(&a.0).unwrap();
        assert!(d.samples[10] > 0.999 && d.samples[11] < -0.999 && d.samples[12].abs() < 0.001);
        // Float: the bits are the samples (no dither, no rounding).
        let f = encode(&x[..10], 2, 48_000, true);
        let (back, _) = decode(&f).unwrap();
        echlub_rt_testkit::bits::assert_same_bits(&back.samples, &x[..10], "32-bit float");
        assert_eq!(f.len(), 44 + 40);
    }

    // --- Malformed and truncated input: an error or a shorter decode, never a panic ---

    /// A RIFF/WAVE file from chunks; odd-sized chunks are padded as the format says.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WAVE".to_vec();
        for (id, body) in chunks {
            b.extend_from_slice(*id);
            b.extend_from_slice(&(body.len() as u32).to_le_bytes());
            b.extend_from_slice(body);
            if body.len() % 2 == 1 {
                b.push(0);
            }
        }
        let n = b.len() as u32 - 8;
        b[4..8].copy_from_slice(&n.to_le_bytes());
        b
    }

    /// A `fmt ` chunk body: the 16 standard bytes, plus the extensible tail when `sub` is given.
    fn fmt_body(tag: u16, ch: u16, rate: u32, bits: u16, sub: Option<u16>) -> Vec<u8> {
        let bpf = ch.wrapping_mul(bits / 8);
        let mut f = Vec::new();
        for v in [tag, ch] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&rate.wrapping_mul(bpf as u32).to_le_bytes());
        f.extend_from_slice(&bpf.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        if let Some(sub) = sub {
            f.extend_from_slice(&22u16.to_le_bytes()); // cbSize
            f.extend_from_slice(&bits.to_le_bytes()); // valid bits
            f.extend_from_slice(&3u32.to_le_bytes()); // channel mask
            f.extend_from_slice(&sub.to_le_bytes()); // sub-format: the first two bytes of the GUID
            f.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
        }
        f
    }

    fn data_bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 37 % 251) as u8).collect()
    }

    /// Valid files of every layout the decoder reads, with its rate and channel count.
    fn valid_files() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("16-bit stereo", riff(&[(b"fmt ", fmt_body(1, 2, 44_100, 16, None)), (b"data", data_bytes(40))])),
            ("24-bit mono", riff(&[(b"fmt ", fmt_body(1, 1, 48_000, 24, None)), (b"data", data_bytes(30))])),
            ("32-bit integer stereo", riff(&[(b"fmt ", fmt_body(1, 2, 96_000, 32, None)), (b"data", data_bytes(48))])),
            ("32-bit float stereo", riff(&[(b"fmt ", fmt_body(3, 2, 48_000, 32, None)), (b"data", data_bytes(32))])),
            ("extensible 24-bit", riff(&[(b"fmt ", fmt_body(0xFFFE, 2, 48_000, 24, Some(1))), (b"data", data_bytes(36))])),
            ("extensible float", riff(&[(b"fmt ", fmt_body(0xFFFE, 1, 48_000, 32, Some(3))), (b"data", data_bytes(16))])),
            (
                "odd chunk sizes, a chunk before and after the audio, a trailing odd data length",
                riff(&[
                    (b"LIST", vec![1, 2, 3]),
                    (b"fmt ", fmt_body(1, 1, 22_050, 16, None)),
                    (b"junk", vec![9]),
                    (b"data", data_bytes(15)),
                    (b"smpl", vec![0; 36]),
                ]),
            ),
            ("six channels folded to stereo", riff(&[(b"fmt ", fmt_body(1, 6, 48_000, 16, None)), (b"data", data_bytes(48))])),
        ]
    }

    #[test]
    fn every_prefix_of_a_valid_file_is_an_error_or_a_shorter_decode() {
        for (what, full) in valid_files() {
            let (whole, rate) = decode(&full).unwrap_or_else(|e| panic!("{what}: the valid file must decode: {e:?}"));
            for n in 0..full.len() {
                if let Ok((d, r)) = decode(&full[..n]) {
                    assert_eq!((d.channels, r), (whole.channels, rate), "{what}, prefix {n}");
                    assert!(d.samples.len() <= whole.samples.len() && d.samples == whole.samples[..d.samples.len()], "{what}, prefix {n}: not a prefix of the full decode");
                }
            }
        }
    }

    #[test]
    fn hostile_headers_are_refused_not_followed() {
        let ok_data = || (b"data", data_bytes(16));
        // `fmt ` shorter than 16 bytes, at the end of the file and followed by a chunk.
        for len in [0usize, 1, 4, 14, 15] {
            let at_end = riff(&[(b"fmt ", vec![1, 0, 2, 0, 0x44, 0xAC, 0, 0, 0, 0, 0, 0, 4, 0, 16][..len.min(15)].to_vec())]);
            assert!(decode(&at_end).is_err(), "fmt of {len} bytes at the end of the file");
            let before_data = riff(&[(b"fmt ", vec![0; len]), ok_data()]);
            assert!(decode(&before_data).is_err(), "fmt of {len} bytes before the data");
        }
        // Zero channels.
        let zero_ch = riff(&[(b"fmt ", fmt_body(1, 0, 48_000, 16, None)), ok_data()]);
        assert!(decode(&zero_ch).is_err(), "zero channels");
        // Zero block align and byte rate: they are not used, so the audio still decodes.
        let mut zero_align = riff(&[(b"fmt ", fmt_body(1, 2, 48_000, 16, None)), ok_data()]);
        zero_align[28..34].fill(0); // byte rate and block align
        assert_eq!(decode(&zero_align).unwrap().0.frames(), 4);
        // Zero sample rate is the caller's business; it must not panic.
        let _ = decode(&riff(&[(b"fmt ", fmt_body(1, 2, 0, 16, None)), ok_data()]));
        // A chunk length larger than the file: for `fmt ` an error, for `data` what is there.
        let mut big_fmt = riff(&[(b"fmt ", fmt_body(1, 2, 48_000, 16, None)), ok_data()]);
        big_fmt[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&big_fmt), Err(WavError::Truncated));
        let mut big_data = riff(&[(b"fmt ", fmt_body(1, 2, 48_000, 16, None)), ok_data()]);
        let at = big_data.len() - 16 - 4;
        big_data[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&big_data).unwrap().0.frames(), 4, "a huge declared data length reads what is there");
        let mut huge_other = riff(&[(b"LIST", vec![0; 4]), (b"fmt ", fmt_body(1, 2, 48_000, 16, None)), ok_data()]);
        huge_other[16..20].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(decode(&huge_other).is_err(), "a skipped chunk longer than the file");
        // An extensible header whose tail is cut short, and bit depths and formats the decoder does not read.
        let mut short_ext = riff(&[(b"fmt ", fmt_body(0xFFFE, 2, 48_000, 24, Some(1))[..20].to_vec()), ok_data()]);
        assert!(decode(&short_ext).is_err());
        short_ext.clear();
        for (tag, bits) in [(1u16, 0u16), (1, 8), (1, 12), (3, 16), (3, 64), (2, 4), (0, 16), (0xFFFF, 16)] {
            let _ = decode(&riff(&[(b"fmt ", fmt_body(tag, 2, 48_000, bits, None)), ok_data()]));
        }
        // Data before `fmt `, and no data chunk at all.
        assert!(decode(&riff(&[ok_data(), (b"fmt ", fmt_body(1, 2, 48_000, 16, None))])).is_err());
        assert!(decode(&riff(&[(b"fmt ", fmt_body(1, 2, 48_000, 16, None))])).is_err());
        // Only a header's worth of bytes, and RIFF chunks that claim to start past the end.
        for n in 0..=12 {
            let _ = decode(&b"RIFF\0\0\0\0WAVEfmt "[..n.min(16)]);
        }
        let _ = decode(b"RIFF\0\0\0\0WAVEfmt \x04\0\0\0abcd");
    }

    #[test]
    fn smpl_chunk_prefixes_and_hostile_lengths_do_not_panic() {
        let x: Vec<f32> = (0..100).map(|i| (i as f32 * 0.1).sin()).collect();
        let info = SmplInfo { unity_note: 60, loops: vec![(10, 50), (20, 90)] };
        let full = with_smpl(encode(&x, 1, 48_000, false), &info);
        for n in 0..full.len() {
            let _ = smpl(&full[..n]);
            let _ = decode(&full[..n]);
        }
        let mut hostile = with_smpl(encode(&x, 1, 48_000, false), &info);
        let at = hostile.len() - (36 + 2 * 24) + 28;
        hostile[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(smpl(&hostile).map(|s| s.loops.len()), Some(2), "a loop count larger than the chunk reads the loops that are there");
    }
}
