//! Core Audio Format (CAF) in: uncompressed linear PCM only (`lpcm`:
//! 8/16/24/32-bit integer, big- or little-endian, and 32/64-bit float).
//! Compressed CAF (ALAC, AAC, IMA and the like) is refused with the codec's
//! name. Same output as [`crate::wav`]: interleaved samples, 1 or 2
//! channels (more are folded to stereo), and the sample rate. Allocates;
//! not for the audio thread.

use crate::wav::Decoded;

#[derive(Debug, PartialEq, Eq)]
pub enum CafError {
    NotCaf,
    /// A codec or layout this reader does not decode (the text names it).
    Unsupported(String),
    Truncated,
}

/// `desc` chunk flags.
const FLOAT: u32 = 1;
const LITTLE: u32 = 2;

/// The `desc` chunk's fields.
struct Desc {
    rate: f64,
    format: [u8; 4],
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    channels: u32,
    bits: u32,
}

/// Decoded CAF: the audio and its sample rate.
pub fn decode(bytes: &[u8]) -> Result<(Decoded, u32), CafError> {
    if bytes.len() < 8 || &bytes[0..4] != b"caff" {
        return Err(CafError::NotCaf);
    }
    let be32 = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let mut desc: Option<Desc> = None;
    let mut data: Option<(usize, usize)> = None;
    let mut i = 8;
    while i + 12 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = i64::from_be_bytes(bytes[i + 4..i + 12].try_into().expect("8 bytes"));
        let body = i + 12;
        // The data chunk may say -1: it runs to the end of the file.
        let end = if size < 0 { bytes.len() } else { body.saturating_add(usize::try_from(size).unwrap_or(usize::MAX)) };
        if id == b"desc" {
            if end > bytes.len() || size < 32 {
                return Err(CafError::Truncated);
            }
            let rate = f64::from_be_bytes(bytes[body..body + 8].try_into().expect("8 bytes"));
            let format: [u8; 4] = bytes[body + 8..body + 12].try_into().expect("4 bytes");
            desc = Some(Desc { rate, format, flags: be32(body + 12), bytes_per_packet: be32(body + 16), frames_per_packet: be32(body + 20), channels: be32(body + 24), bits: be32(body + 28) });
        } else if id == b"data" {
            // Edit count first, then the audio (a cut-off file reads as shorter).
            if body + 4 > bytes.len() {
                return Err(CafError::Truncated);
            }
            data = Some((body + 4, end.min(bytes.len())));
        }
        if size < 0 || end >= bytes.len() {
            break;
        }
        i = end;
    }
    let Desc { rate, format, flags, bytes_per_packet, frames_per_packet, channels: ch, bits } = desc.ok_or(CafError::Unsupported("no desc chunk".into()))?;
    if &format != b"lpcm" {
        return Err(CafError::Unsupported(format!("CAF codec {} (compressed)", String::from_utf8_lossy(&format).trim())));
    }
    let (start, end) = data.ok_or(CafError::Unsupported("no data chunk".into()))?;
    let ch = ch as usize;
    if ch == 0 || !(1.0..=1.0e6).contains(&rate) || frames_per_packet != 1 {
        return Err(CafError::Unsupported(format!("{ch} channels at {rate} Hz, {frames_per_packet} frames per packet")));
    }
    let float = flags & FLOAT != 0;
    let little = flags & LITTLE != 0;
    let step = match (float, bits) {
        (true, 32 | 64) | (false, 8 | 16 | 24 | 32) => bits as usize / 8,
        (true, b) => return Err(CafError::Unsupported(format!("{b}-bit float"))),
        (false, b) => return Err(CafError::Unsupported(format!("{b}-bit PCM"))),
    };
    // A channel count too large for the product is no packed layout either.
    let frame_bytes = step.checked_mul(ch).filter(|&n| n == bytes_per_packet as usize);
    let Some(frame_bytes) = frame_bytes else {
        return Err(CafError::Unsupported(format!("{bytes_per_packet} bytes per frame for {ch} x {bits} bits (packed samples only)")));
    };
    let read = |b: &[u8]| -> f32 {
        let w4 = |b: &[u8]| -> [u8; 4] { if little { [b[0], b[1], b[2], b[3]] } else { [b[3], b[2], b[1], b[0]] } };
        match (float, step) {
            (true, 4) => f32::from_le_bytes(w4(b)),
            (true, _) => {
                let mut w: [u8; 8] = b[..8].try_into().expect("8 bytes");
                if !little {
                    w.reverse();
                }
                f64::from_le_bytes(w) as f32
            }
            (false, 1) => b[0] as i8 as f32 / 128.0,
            (false, 2) => (if little { i16::from_le_bytes([b[0], b[1]]) } else { i16::from_be_bytes([b[0], b[1]]) }) as f32 / 32768.0,
            (false, 3) => {
                let v = if little { i32::from_le_bytes([0, b[0], b[1], b[2]]) } else { i32::from_be_bytes([b[0], b[1], b[2], 0]) };
                (v >> 8) as f32 / 8_388_608.0
            }
            (false, _) => i32::from_le_bytes(w4(b)) as f32 / 2_147_483_648.0,
        }
    };
    let audio = &bytes[start..end.max(start)];
    let n = audio.len() / frame_bytes;
    let out_ch = ch.min(2);
    let mut samples = Vec::with_capacity(n * out_ch);
    for f in 0..n {
        let at = |c: usize| read(&audio[(f * ch + c) * step..]);
        if ch <= 2 {
            for c in 0..ch {
                samples.push(at(c));
            }
        } else {
            let (mut l, mut r) = (0.0, 0.0);
            for c in 0..ch {
                if c % 2 == 0 {
                    l += at(c)
                } else {
                    r += at(c)
                }
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

    /// A CAF file: `desc` for `format`, then a `data` chunk of `body`
    /// (`size` -1 when `open_ended`), with a `free` chunk between.
    fn caf(rate: f64, format: &[u8; 4], flags: u32, ch: u32, bits: u32, body: &[u8], open_ended: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"caff");
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(b"desc");
        out.extend_from_slice(&32i64.to_be_bytes());
        out.extend_from_slice(&rate.to_be_bytes());
        out.extend_from_slice(format);
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&(ch * bits / 8).to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&ch.to_be_bytes());
        out.extend_from_slice(&bits.to_be_bytes());
        out.extend_from_slice(b"free");
        out.extend_from_slice(&3i64.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(if open_ended { -1 } else { body.len() as i64 + 4 }).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn pcm_and_float_in_both_byte_orders() {
        // 16-bit big-endian stereo (as in some Logic Pro instrument files): 0.5, -0.5.
        let (d, r) = decode(&caf(44_100.0, b"lpcm", 0, 2, 16, &[0x40, 0x00, 0xC0, 0x00], false)).unwrap();
        assert_eq!((d.channels, r, d.samples.clone()), (2, 44_100, vec![0.5, -0.5]));
        // 16-bit little-endian, open-ended data chunk.
        let (d, _) = decode(&caf(48_000.0, b"lpcm", LITTLE, 1, 16, &[0x00, 0x40, 0x00, 0xE0], true)).unwrap();
        assert_eq!(d.samples, vec![0.5, -0.25]);
        // 24-bit, both orders: 0.25 and -0.5.
        let (d, _) = decode(&caf(48_000.0, b"lpcm", 0, 1, 24, &[0x20, 0x00, 0x00, 0xC0, 0x00, 0x00], false)).unwrap();
        assert_eq!(d.samples, vec![0.25, -0.5]);
        let (d, _) = decode(&caf(48_000.0, b"lpcm", LITTLE, 1, 24, &[0x00, 0x00, 0x20, 0x00, 0x00, 0xC0], false)).unwrap();
        assert_eq!(d.samples, vec![0.25, -0.5]);
        // 32-bit integer and float, 64-bit float.
        let (d, _) = decode(&caf(48_000.0, b"lpcm", 0, 1, 32, &0x4000_0000i32.to_be_bytes(), false)).unwrap();
        assert_eq!(d.samples, vec![0.5]);
        let (d, _) = decode(&caf(48_000.0, b"lpcm", FLOAT, 1, 32, &0.75f32.to_be_bytes(), false)).unwrap();
        assert_eq!(d.samples, vec![0.75]);
        let (d, _) = decode(&caf(48_000.0, b"lpcm", FLOAT | LITTLE, 1, 64, &(-0.125f64).to_le_bytes(), false)).unwrap();
        assert_eq!(d.samples, vec![-0.125]);
        // 8-bit signed.
        let (d, _) = decode(&caf(48_000.0, b"lpcm", 0, 1, 8, &[0x80, 0x40], false)).unwrap();
        assert_eq!(d.samples, vec![-1.0, 0.5]);
        // Four channels fold to stereo (even to left, odd to right).
        let body: Vec<u8> = [0.5f32, 0.25, 0.1, 0.05].iter().flat_map(|v| v.to_be_bytes()).collect();
        let (d, _) = decode(&caf(48_000.0, b"lpcm", FLOAT, 4, 32, &body, false)).unwrap();
        assert_eq!(d.channels, 2);
        assert!((d.samples[0] - 0.3).abs() < 1e-6 && (d.samples[1] - 0.15).abs() < 1e-6);
    }

    #[test]
    fn compressed_and_broken_files_are_refused() {
        assert_eq!(decode(b"RIFF....WAVE"), Err(CafError::NotCaf));
        match decode(&caf(44_100.0, b"alac", 0, 2, 16, &[0; 16], false)) {
            Err(CafError::Unsupported(w)) => assert!(w.contains("alac"), "{w}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(decode(&caf(44_100.0, b"lpcm", 0, 1, 12, &[0; 4], false)), Err(CafError::Unsupported(_))));
        // Every truncation is an error or a shorter sample, never a panic.
        let full = caf(44_100.0, b"lpcm", 0, 2, 24, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], false);
        for n in 0..full.len() {
            let _ = decode(&full[..n]);
        }
        let short = &full[..full.len() - 3];
        assert_eq!(decode(short).unwrap().0.samples.len(), 2, "the whole frames that are there");
    }

    /// Hostile headers: chunk sizes beyond the file or negative, no channels,
    /// huge channel counts, odd packet sizes, a rate that is NaN. Each is an
    /// error or a shorter decode, never a panic.
    #[test]
    fn hostile_headers_do_not_panic() {
        let good = caf(44_100.0, b"lpcm", 0, 2, 16, &[0x40, 0, 0xC0, 0, 0x10, 0, 0x20, 0], false);
        let desc = 8; // offset of the `desc` chunk header
        let body = desc + 12;
        let free = body + 32;
        let data = free + 12 + 3;
        assert_eq!(&good[data..data + 4], b"data");
        type Edit = Box<dyn Fn(&mut Vec<u8>)>;
        let tries: Vec<(&str, Edit)> = vec![
            ("desc longer than the file", Box::new(move |b| b[desc + 4..desc + 12].copy_from_slice(&i64::MAX.to_be_bytes()))),
            ("desc size negative", Box::new(move |b| b[desc + 4..desc + 12].copy_from_slice(&(-5i64).to_be_bytes()))),
            ("desc shorter than 32", Box::new(move |b| b[desc + 4..desc + 12].copy_from_slice(&8i64.to_be_bytes()))),
            ("free chunk size huge", Box::new(move |b| b[free + 4..free + 12].copy_from_slice(&i64::MAX.to_be_bytes()))),
            ("data size huge", Box::new(move |b| b[data + 4..data + 12].copy_from_slice(&i64::MAX.to_be_bytes()))),
            ("data size smaller than its edit count", Box::new(move |b| b[data + 4..data + 12].copy_from_slice(&1i64.to_be_bytes()))),
            ("data size zero", Box::new(move |b| b[data + 4..data + 12].copy_from_slice(&0i64.to_be_bytes()))),
            ("zero channels", Box::new(move |b| b[body + 24..body + 28].copy_from_slice(&0u32.to_be_bytes()))),
            ("huge channels", Box::new(move |b| b[body + 24..body + 28].copy_from_slice(&u32::MAX.to_be_bytes()))),
            ("huge bytes per packet", Box::new(move |b| b[body + 16..body + 20].copy_from_slice(&u32::MAX.to_be_bytes()))),
            ("zero bytes per packet", Box::new(move |b| b[body + 16..body + 20].copy_from_slice(&0u32.to_be_bytes()))),
            ("zero frames per packet", Box::new(move |b| b[body + 20..body + 24].copy_from_slice(&0u32.to_be_bytes()))),
            ("zero bits", Box::new(move |b| b[body + 28..body + 32].copy_from_slice(&0u32.to_be_bytes()))),
            ("NaN rate", Box::new(move |b| b[body..body + 8].copy_from_slice(&f64::NAN.to_be_bytes()))),
            ("zero rate", Box::new(move |b| b[body..body + 8].copy_from_slice(&0f64.to_be_bytes()))),
        ];
        for (what, edit) in tries {
            let mut b = good.clone();
            edit(&mut b);
            let _ = decode(&b);
            for n in 0..b.len() {
                let _ = decode(&b[..n]);
            }
            let _ = what;
        }
        assert!(decode(&good).is_ok());
    }
}
