//! AIFF and AIFF-C in: 8/16/24/32-bit PCM (big-endian, or little-endian
//! `sowt`) and 32/64-bit float (`fl32`, `fl64`). Compressed AIFF-C
//! (ALAC, IMA and the like) is refused. Same output as [`crate::wav`]:
//! interleaved samples, 1 or 2 channels (more are folded to stereo), and
//! the sample rate. Allocates; not for the audio thread.

use crate::wav::Decoded;

#[derive(Debug, PartialEq, Eq)]
pub enum AiffError {
    NotAiff,
    Unsupported(String),
    Truncated,
}

/// The 80-bit IEEE extended float of the COMM chunk's rate.
fn extended(b: &[u8]) -> f64 {
    let exp = (((b[0] & 0x7f) as i32) << 8 | b[1] as i32) - 16383;
    let mant = u64::from_be_bytes([b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]]);
    let v = mant as f64 * 2f64.powi(exp - 63);
    if b[0] & 0x80 != 0 {
        -v
    } else {
        v
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Enc {
    Big,
    Little,
    Float,
}

/// Decoded AIFF / AIFF-C: the audio and its sample rate.
pub fn decode(bytes: &[u8]) -> Result<(Decoded, u32), AiffError> {
    if bytes.len() < 12 || &bytes[0..4] != b"FORM" || !(&bytes[8..12] == b"AIFF" || &bytes[8..12] == b"AIFC") {
        return Err(AiffError::NotAiff);
    }
    let aifc = &bytes[8..12] == b"AIFC";
    let be16 = |i: usize| i16::from_be_bytes([bytes[i], bytes[i + 1]]);
    let be32 = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let mut comm: Option<(usize, u32, usize, f64, Enc)> = None;
    let mut ssnd: Option<(usize, usize)> = None;
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = be32(i + 4) as usize;
        let body = i + 8;
        // `None`: the declared end does not fit in a `usize` (a 32-bit target).
        let end = body.checked_add(len);
        if end.is_none_or(|e| e > bytes.len()) && id != b"SSND" {
            return Err(AiffError::Truncated);
        }
        if id == b"COMM" {
            if len < 18 {
                return Err(AiffError::Truncated);
            }
            let ch = be16(body).max(0) as usize;
            let frames = be32(body + 2);
            let bits = be16(body + 6).max(0) as usize;
            let rate = extended(&bytes[body + 8..body + 18]);
            let enc = if aifc && len >= 22 {
                match &bytes[body + 18..body + 22] {
                    b"NONE" | b"twos" => Enc::Big,
                    b"sowt" => Enc::Little,
                    b"fl32" | b"FL32" | b"fl64" | b"FL64" => Enc::Float,
                    other => return Err(AiffError::Unsupported(format!("AIFF-C compression {}", String::from_utf8_lossy(other)))),
                }
            } else {
                Enc::Big
            };
            comm = Some((ch, frames, bits, rate, enc));
        } else if id == b"SSND" {
            if len < 8 || body + 8 > bytes.len() {
                return Err(AiffError::Truncated);
            }
            let offset = be32(body) as usize;
            let start = (body + 8).saturating_add(offset).min(bytes.len());
            ssnd = Some((start, end.map_or(bytes.len(), |e| e.min(bytes.len()))));
        }
        match end.and_then(|e| e.checked_add(len & 1)) {
            Some(next) => i = next,
            None => break,
        }
    }
    let (ch, frames, bits, rate, enc) = comm.ok_or(AiffError::Unsupported("no COMM chunk".into()))?;
    let (start, end) = ssnd.ok_or(AiffError::Unsupported("no SSND chunk".into()))?;
    if ch == 0 || !(1.0..=1.0e6).contains(&rate) {
        return Err(AiffError::Unsupported(format!("{ch} channels at {rate} Hz")));
    }
    let step = match (enc, bits) {
        (Enc::Float, 32) | (Enc::Float, 64) => bits / 8,
        (Enc::Float, b) => return Err(AiffError::Unsupported(format!("{b}-bit float"))),
        (_, 8 | 16 | 24 | 32) => bits / 8,
        (_, b) => return Err(AiffError::Unsupported(format!("{b}-bit PCM"))),
    };
    let read = |b: &[u8]| -> f32 {
        match (enc, step) {
            (Enc::Float, 4) => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            (Enc::Float, _) => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
            (Enc::Big, 1) | (Enc::Little, 1) => b[0] as i8 as f32 / 128.0,
            (Enc::Big, 2) => i16::from_be_bytes([b[0], b[1]]) as f32 / 32768.0,
            (Enc::Little, 2) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            (Enc::Big, 3) => (i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8) as f32 / 8_388_608.0,
            (Enc::Little, 3) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (Enc::Big, _) => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            (Enc::Little, _) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        }
    };
    let data = &bytes[start..end.max(start)];
    let n = (data.len() / (step * ch)).min(frames as usize);
    let out_ch = ch.min(2);
    let mut samples = Vec::with_capacity(n * out_ch);
    for f in 0..n {
        let at = |c: usize| read(&data[(f * ch + c) * step..]);
        if ch <= 2 {
            for c in 0..ch {
                samples.push(at(c));
            }
        } else {
            let (mut l, mut r) = (0.0, 0.0);
            for c in 0..ch {
                if c % 2 == 0 { l += at(c) } else { r += at(c) }
            }
            let k = (ch / 2) as f32;
            samples.push(l / k);
            samples.push(r / k);
        }
    }
    Ok((Decoded { channels: out_ch as u16, samples }, rate.round() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An AIFF (or AIFF-C with `comp`) of `frames` frames of `body` bytes.
    fn aiff(ch: u16, bits: u16, rate_ext: [u8; 10], comp: Option<&[u8; 4]>, body: &[u8]) -> Vec<u8> {
        let frames = body.len() as u32 / (ch as u32 * bits as u32 / 8);
        let mut comm = Vec::new();
        comm.extend_from_slice(&ch.to_be_bytes());
        comm.extend_from_slice(&frames.to_be_bytes());
        comm.extend_from_slice(&bits.to_be_bytes());
        comm.extend_from_slice(&rate_ext);
        if let Some(c) = comp {
            comm.extend_from_slice(c);
            comm.extend_from_slice(&[0, 0]); // empty pascal name, padded
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"FORM");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(if comp.is_some() { b"AIFC" } else { b"AIFF" });
        out.extend_from_slice(b"COMM");
        out.extend_from_slice(&(comm.len() as u32).to_be_bytes());
        out.extend_from_slice(&comm);
        out.extend_from_slice(b"SSND");
        out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(body);
        let total = out.len() as u32 - 8;
        out[4..8].copy_from_slice(&total.to_be_bytes());
        out
    }

    // 44100 and 48000 as 80-bit extended floats.
    const R44: [u8; 10] = [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0];
    const R48: [u8; 10] = [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0];

    #[test]
    fn pcm_formats_and_rates() {
        assert_eq!(extended(&R44), 44_100.0);
        assert_eq!(extended(&R48), 48_000.0);
        // 16-bit big-endian stereo: 0.5, -0.5.
        let (d, r) = decode(&aiff(2, 16, R44, None, &[0x40, 0x00, 0xC0, 0x00])).unwrap();
        assert_eq!((d.channels, r, d.samples.clone()), (2, 44_100, vec![0.5, -0.5]));
        // 24-bit big-endian mono: 0.25.
        let (d, _) = decode(&aiff(1, 24, R48, None, &[0x20, 0x00, 0x00])).unwrap();
        assert_eq!(d.samples, vec![0.25]);
        // 8-bit signed.
        let (d, _) = decode(&aiff(1, 8, R48, None, &[0x80, 0x40])).unwrap();
        assert_eq!(d.samples, vec![-1.0, 0.5]);
        // AIFF-C little-endian 16 (sowt) and big-endian float.
        let (d, _) = decode(&aiff(1, 16, R48, Some(b"sowt"), &[0x00, 0x40])).unwrap();
        assert_eq!(d.samples, vec![0.5]);
        let (d, _) = decode(&aiff(1, 32, R48, Some(b"fl32"), &0.75f32.to_be_bytes())).unwrap();
        assert_eq!(d.samples, vec![0.75]);
        let (d, _) = decode(&aiff(1, 32, R48, None, &0x4000_0000i32.to_be_bytes())).unwrap();
        assert_eq!(d.samples, vec![0.5]);
    }

    #[test]
    fn refusals() {
        assert_eq!(decode(b"RIFF....WAVE"), Err(AiffError::NotAiff));
        assert!(matches!(decode(&aiff(1, 16, R48, Some(b"alac"), &[0, 0])), Err(AiffError::Unsupported(_))));
        let mut t = aiff(1, 16, R48, None, &[0, 0, 0, 0]);
        t.truncate(30);
        assert!(decode(&t).is_err());
        // Every truncation of a valid file is an error or a shorter sample, never a panic.
        let full = aiff(2, 24, R44, None, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        for n in 0..full.len() {
            let _ = decode(&full[..n]);
        }
    }

    /// Hostile headers: a chunk longer than the file, no channels, no bits,
    /// an impossible rate, a short COMM, an SSND offset past the end, huge
    /// frame counts. Each is an error or a shorter decode, never a panic.
    #[test]
    fn hostile_headers_do_not_panic() {
        let good = aiff(2, 16, R44, None, &[0x40, 0x00, 0xC0, 0x00, 0x10, 0x00, 0x20, 0x00]);
        let comm = 12; // offset of the COMM chunk header
        let ssnd = comm + 8 + 18;
        assert_eq!(&good[ssnd..ssnd + 4], b"SSND");
        type Edit = Box<dyn Fn(&mut Vec<u8>)>;
        let tries: Vec<(&str, Edit)> = vec![
            ("COMM longer than the file", Box::new(move |b| b[comm + 4..comm + 8].copy_from_slice(&u32::MAX.to_be_bytes()))),
            ("SSND longer than the file", Box::new(move |b| b[ssnd + 4..ssnd + 8].copy_from_slice(&u32::MAX.to_be_bytes()))),
            ("COMM shorter than 18", Box::new(move |b| b[comm + 4..comm + 8].copy_from_slice(&4u32.to_be_bytes()))),
            ("zero channels", Box::new(move |b| b[comm + 8..comm + 10].copy_from_slice(&0u16.to_be_bytes()))),
            ("negative channels", Box::new(move |b| b[comm + 8..comm + 10].copy_from_slice(&0xFFFFu16.to_be_bytes()))),
            ("zero bits", Box::new(move |b| b[comm + 14..comm + 16].copy_from_slice(&0u16.to_be_bytes()))),
            ("huge frame count", Box::new(move |b| b[comm + 10..comm + 14].copy_from_slice(&u32::MAX.to_be_bytes()))),
            ("zero rate", Box::new(move |b| b[comm + 16..comm + 26].fill(0))),
            ("NaN rate", Box::new(move |b| b[comm + 16..comm + 26].copy_from_slice(&[0x7F, 0xFF, 0xC0, 0, 0, 0, 0, 0, 0, 0]))),
            ("huge exponent", Box::new(move |b| b[comm + 16..comm + 18].copy_from_slice(&[0x7F, 0xFE]))),
            ("SSND offset past the end", Box::new(move |b| b[ssnd + 8..ssnd + 12].copy_from_slice(&u32::MAX.to_be_bytes()))),
            ("SSND shorter than 8", Box::new(move |b| b[ssnd + 4..ssnd + 8].copy_from_slice(&3u32.to_be_bytes()))),
            ("FORM size lies", Box::new(|b| b[4..8].copy_from_slice(&u32::MAX.to_be_bytes()))),
        ];
        for (what, edit) in tries {
            let mut b = good.clone();
            edit(&mut b);
            let _ = decode(&b);
            // And the same with the data cut at every length.
            for n in 0..b.len() {
                let _ = decode(&b[..n]);
            }
            let _ = what;
        }
        assert!(decode(&good).is_ok());
    }
}
