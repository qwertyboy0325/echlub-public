//! The FLAC encoder against a decoder written here for exactly the subset
//! it writes (and used only by these tests): bit-exact round trips on
//! generated signals, every CRC and the MD5 checked, then (ignored, run by
//! hand) the system's own decoders as an independent witness.

use super::*;

// --- The reference decoder, from the format's specification ---------------

fn crc8_bitwise(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
        }
    }
    c
}

fn crc16_bitwise(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
        }
    }
    c
}

struct Reader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl Reader<'_> {
    fn bits(&mut self, n: u32) -> Result<u64, String> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = *self.data.get(self.bit / 8).ok_or("ran out of data")?;
            v = v << 1 | ((byte >> (7 - self.bit % 8)) & 1) as u64;
            self.bit += 1;
        }
        Ok(v)
    }
    fn signed(&mut self, n: u32) -> Result<i64, String> {
        let v = self.bits(n)?;
        Ok(if n > 0 && v >> (n - 1) == 1 { v as i64 - (1i64 << n) } else { v as i64 })
    }
    fn aligned(&self) -> bool {
        self.bit.is_multiple_of(8)
    }
}

#[derive(Debug, PartialEq)]
struct Decoded {
    channels: u16,
    bits: u8,
    rate: u32,
    samples: Vec<i32>,
    frame_sizes: Vec<usize>,
    block_sizes: Vec<usize>,
    /// Per frame: the channel assignment code and each subframe's type code.
    modes: Vec<(u8, Vec<u8>)>,
}

fn residual(r: &mut Reader, n: usize, order: usize) -> Result<Vec<i64>, String> {
    let method = r.bits(2)?;
    let width = match method {
        0 => 4,
        1 => 5,
        m => return Err(format!("residual coding method {m}")),
    };
    let porder = r.bits(4)? as u32;
    if !n.is_multiple_of(1 << porder) || (n >> porder) <= order {
        return Err("bad partition order".into());
    }
    let mut out = Vec::new();
    for i in 0..(1usize << porder) {
        let k = r.bits(width)? as u32;
        if k == (1 << width) - 1 {
            return Err("escaped partition (not in the subset)".into());
        }
        let count = (n >> porder) - if i == 0 { order } else { 0 };
        for _ in 0..count {
            let mut q = 0u64;
            while r.bits(1)? == 0 {
                q += 1;
            }
            let u = q << k | r.bits(k)?;
            out.push(if u & 1 == 0 { (u >> 1) as i64 } else { -((u >> 1) as i64) - 1 });
        }
    }
    Ok(out)
}

fn subframe(r: &mut Reader, n: usize, bps: u32) -> Result<(Vec<i64>, u8), String> {
    if r.bits(1)? != 0 {
        return Err("subframe padding bit".into());
    }
    let ty = r.bits(6)? as u8;
    let mut wasted = 0u32;
    if r.bits(1)? == 1 {
        wasted = 1;
        while r.bits(1)? == 0 {
            wasted += 1;
        }
    }
    let bps = bps - wasted;
    let mut x: Vec<i64> = Vec::with_capacity(n);
    match ty {
        0 => {
            let v = r.signed(bps)?;
            x.resize(n, v);
        }
        1 => {
            for _ in 0..n {
                x.push(r.signed(bps)?);
            }
        }
        8..=12 => {
            let order = (ty - 8) as usize;
            for _ in 0..order {
                x.push(r.signed(bps)?);
            }
            let res = residual(r, n, order)?;
            for (i, e) in res.into_iter().enumerate() {
                let t = i + order;
                let p = match order {
                    0 => 0,
                    1 => x[t - 1],
                    2 => 2 * x[t - 1] - x[t - 2],
                    3 => 3 * x[t - 1] - 3 * x[t - 2] + x[t - 3],
                    _ => 4 * x[t - 1] - 6 * x[t - 2] + 4 * x[t - 3] - x[t - 4],
                };
                x.push(p + e);
            }
        }
        32..=63 => {
            let order = (ty - 31) as usize;
            for _ in 0..order {
                x.push(r.signed(bps)?);
            }
            let precision = r.bits(4)? as u32 + 1;
            if precision == 16 {
                return Err("invalid coefficient precision".into());
            }
            let shift = r.signed(5)?;
            if shift < 0 {
                return Err("negative shift".into());
            }
            let coefs: Vec<i64> = (0..order).map(|_| r.signed(precision)).collect::<Result<_, _>>()?;
            let res = residual(r, n, order)?;
            for (i, e) in res.into_iter().enumerate() {
                let t = i + order;
                let s: i64 = coefs.iter().enumerate().map(|(j, c)| c * x[t - 1 - j]).sum();
                x.push((s >> shift) + e);
            }
        }
        other => return Err(format!("subframe type {other}")),
    }
    for v in &mut x {
        *v <<= wasted;
    }
    Ok((x, ty))
}

fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    if bytes.len() < 42 || &bytes[..4] != b"fLaC" {
        return Err("no fLaC marker".into());
    }
    if bytes[4] != 0x80 || bytes[5..8] != [0, 0, 34] {
        return Err("STREAMINFO is not the only, last metadata block".into());
    }
    let si = &bytes[8..42];
    let (min_bs, max_bs) = (u16::from_be_bytes([si[0], si[1]]) as usize, u16::from_be_bytes([si[2], si[3]]) as usize);
    let (min_fs, max_fs) = (u32::from_be_bytes([0, si[4], si[5], si[6]]) as usize, u32::from_be_bytes([0, si[7], si[8], si[9]]) as usize);
    let packed = u64::from_be_bytes([si[10], si[11], si[12], si[13], si[14], si[15], si[16], si[17]]);
    let rate = (packed >> 44) as u32;
    let channels = ((packed >> 41) & 7) as u16 + 1;
    let bits = ((packed >> 36) & 31) as u8 + 1;
    let total = packed & ((1 << 36) - 1);
    let md5_si = &si[18..34];
    let mut at = 42;
    let mut samples: Vec<i32> = Vec::new();
    let (mut frame_sizes, mut block_sizes, mut modes) = (Vec::new(), Vec::new(), Vec::new());
    let mut index = 0u64;
    while at < bytes.len() {
        let start = at;
        let b = &bytes[at..];
        if b.len() < 6 || b[0] != 0xff || b[1] != 0xf8 {
            return Err(format!("no frame sync at {at}"));
        }
        let (size_code, rate_code) = (b[2] >> 4, b[2] & 15);
        let (assign, size_bits, reserved) = (b[3] >> 4, (b[3] >> 1) & 7, b[3] & 1);
        if reserved != 0 {
            return Err("reserved bit".into());
        }
        // The frame number, UTF-8 style.
        let lead = b[4];
        let extra = lead.leading_ones().saturating_sub(1) as usize;
        if lead & 0x80 != 0 && extra == 0 {
            return Err("bad frame number".into());
        }
        let mut number = if extra == 0 { lead as u64 } else { (lead & (0x7f >> (extra + 1))) as u64 };
        for i in 0..extra {
            let c = b[5 + i];
            if c & 0xc0 != 0x80 {
                return Err("bad frame number continuation".into());
            }
            number = number << 6 | (c & 0x3f) as u64;
        }
        if number != index {
            return Err(format!("frame number {number}, expected {index}"));
        }
        let mut hlen = 5 + extra;
        let n = match size_code {
            1 => 192,
            2..=5 => 576 << (size_code - 2),
            6 => {
                hlen += 1;
                b[hlen - 1] as usize + 1
            }
            7 => {
                hlen += 2;
                u16::from_be_bytes([b[hlen - 2], b[hlen - 1]]) as usize + 1
            }
            8..=15 => 256 << (size_code - 8),
            _ => return Err("blocksize code".into()),
        };
        let frame_rate = match rate_code {
            0 => rate,
            1 => 88_200,
            9 => 44_100,
            10 => 48_000,
            11 => 96_000,
            4 => 8_000,
            5 => 16_000,
            6 => 22_050,
            7 => 24_000,
            8 => 32_000,
            2 => 176_400,
            3 => 192_000,
            c => return Err(format!("rate code {c}")),
        };
        if frame_rate != rate {
            return Err("frame rate differs from STREAMINFO".into());
        }
        let frame_bits = match size_bits {
            0 => bits,
            0b100 => 16,
            0b110 => 24,
            c => return Err(format!("sample size code {c}")),
        };
        if frame_bits != bits {
            return Err("frame depth differs from STREAMINFO".into());
        }
        if crc8_bitwise(&b[..hlen]) != b[hlen] {
            return Err("header CRC-8".into());
        }
        let mut r = Reader { data: bytes, bit: (at + hlen + 1) * 8 };
        let ch_count = match assign {
            0 => 1,
            1 | 8 | 9 | 10 => 2,
            a => return Err(format!("channel assignment {a}")),
        };
        if ch_count != channels as usize {
            return Err("channel count differs from STREAMINFO".into());
        }
        let mut planes = Vec::new();
        let mut types = Vec::new();
        for c in 0..ch_count {
            let side = matches!((assign, c), (8, 1) | (9, 0) | (10, 1));
            let (x, ty) = subframe(&mut r, n, bits as u32 + side as u32)?;
            planes.push(x);
            types.push(ty);
        }
        while !r.aligned() {
            if r.bits(1)? != 0 {
                return Err("nonzero padding".into());
            }
        }
        at = r.bit / 8;
        let crc = u16::from_be_bytes([*bytes.get(at).ok_or("no CRC")?, *bytes.get(at + 1).ok_or("no CRC")?]);
        if crc16_bitwise(&bytes[start..at]) != crc {
            return Err("frame CRC-16".into());
        }
        at += 2;
        let (a, b2) = match assign {
            8 => {
                let (l, s) = (planes[0].clone(), planes[1].clone());
                let r2 = l.iter().zip(&s).map(|(l, s)| l - s).collect();
                (l, r2)
            }
            9 => {
                let (s, r2) = (planes[0].clone(), planes[1].clone());
                let l = r2.iter().zip(&s).map(|(r, s)| r + s).collect();
                (l, r2)
            }
            10 => {
                let (m, s) = (&planes[0], &planes[1]);
                let l: Vec<i64> = m.iter().zip(s).map(|(m, s)| ((m << 1 | (s & 1)) + s) >> 1).collect();
                let r2: Vec<i64> = m.iter().zip(s).map(|(m, s)| ((m << 1 | (s & 1)) - s) >> 1).collect();
                (l, r2)
            }
            _ => (planes[0].clone(), planes.get(1).cloned().unwrap_or_default()),
        };
        for i in 0..n {
            samples.push(a[i] as i32);
            if ch_count == 2 {
                samples.push(b2[i] as i32);
            }
        }
        frame_sizes.push(at - start);
        block_sizes.push(n);
        modes.push((assign, types));
        index += 1;
    }
    if samples.len() as u64 != total * channels as u64 {
        return Err(format!("{} samples, STREAMINFO says {}", samples.len() / channels as usize, total));
    }
    let bytes_per = bits as usize / 8;
    let mut raw = Vec::new();
    for s in &samples {
        raw.extend_from_slice(&s.to_le_bytes()[..bytes_per]);
    }
    if crate::md5::md5(&raw) != md5_si {
        return Err("MD5 differs".into());
    }
    if let Some((&last, rest)) = block_sizes.split_last() {
        if rest.iter().any(|&b| b != max_bs) || last > max_bs || min_bs != max_bs {
            return Err("block sizes are not fixed".into());
        }
    }
    if frame_sizes.iter().copied().min().unwrap_or(0) != min_fs || frame_sizes.iter().copied().max().unwrap_or(0) != max_fs {
        return Err("frame sizes differ from STREAMINFO".into());
    }
    Ok(Decoded { channels, bits, rate, samples, frame_sizes, block_sizes, modes })
}

// --- Generated signals -----------------------------------------------------

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform over the bit depth's whole signed range.
    fn sample(&mut self, bits: u8) -> i32 {
        let span = 1u64 << bits;
        (self.next() % span) as i32 - (1 << (bits - 1))
    }
}

fn full(bits: u8) -> i32 {
    (1 << (bits - 1)) - 1
}

fn sine(n: usize, bits: u8, amp: f64, cycles: f64) -> Vec<i32> {
    (0..n).map(|i| (amp * full(bits) as f64 * (i as f64 * cycles * std::f64::consts::TAU / n as f64).sin()).round() as i32).collect()
}

fn sweep(n: usize, bits: u8) -> Vec<i32> {
    // 20 Hz to 20 kHz at 48 kHz, exponential.
    let (f0, f1, rate) = (20.0f64, 20_000.0f64, 48_000.0f64);
    let t_end = n as f64 / rate;
    let k = (f1 / f0).ln() / t_end;
    (0..n)
        .map(|i| {
            let t = i as f64 / rate;
            let phase = std::f64::consts::TAU * f0 * ((k * t).exp() - 1.0) / k;
            (0.8 * full(bits) as f64 * phase.sin()).round() as i32
        })
        .collect()
}

fn interleave(a: &[i32], b: &[i32]) -> Vec<i32> {
    a.iter().zip(b).flat_map(|(&l, &r)| [l, r]).collect()
}

fn round_trip(samples: &[i32], ch: u16, bits: u8, rate: u32, what: &str) -> Vec<u8> {
    let enc = encode(samples, ch, bits, rate).unwrap_or_else(|e| panic!("{what}: {e}"));
    let d = decode(&enc.bytes).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert_eq!((d.channels, d.bits, d.rate), (ch, bits, rate), "{what}");
    assert!(d.samples == samples, "{what}: samples differ ({} vs {})", d.samples.len(), samples.len());
    let frames = samples.len() / ch as usize;
    assert_eq!(d.block_sizes.len(), frames.div_ceil(BLOCK_SIZE), "{what}");
    enc.bytes
}

/// The signals the encoder is tested on, at a depth, as (name, channels, samples).
fn signals(bits: u8) -> Vec<(String, u16, Vec<i32>)> {
    let mut v: Vec<(String, u16, Vec<i32>)> = Vec::new();
    let n = 20_000;
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    v.push(("silence mono".into(), 1, vec![0; n]));
    v.push(("silence stereo".into(), 2, vec![0; 2 * n]));
    v.push(("full-scale sine".into(), 1, sine(n, bits, 1.0, 97.3)));
    v.push(("quiet sine".into(), 1, sine(n, bits, 0.001, 41.0)));
    v.push(("white noise mono".into(), 1, (0..n).map(|_| noise.sample(bits)).collect()));
    v.push(("white noise stereo".into(), 2, (0..2 * n).map(|_| noise.sample(bits)).collect()));
    v.push(("sweep".into(), 1, sweep(60_000, bits)));
    let l = sine(n, bits, 0.7, 13.0);
    v.push(("stereo identical channels".into(), 2, interleave(&l, &l)));
    let opposite: Vec<i32> = l.iter().map(|&x| -x).collect();
    v.push(("stereo opposite channels".into(), 2, interleave(&l, &opposite)));
    v.push(("stereo sines".into(), 2, interleave(&sine(n, bits, 0.5, 7.0), &sine(n, bits, 0.4, 11.0))));
    v.push(("dc mono".into(), 1, vec![1234; n]));
    v.push(("dc negative extreme".into(), 1, vec![-(1 << (bits - 1)); n]));
    v.push(("dc stereo".into(), 2, interleave(&vec![77; n], &vec![-5000; n])));
    v.push(("full-scale square".into(), 1, (0..n).map(|i| if (i / 50) % 2 == 0 { full(bits) } else { -(1 << (bits - 1)) }).collect()));
    v.push(("extremes alternating stereo".into(), 2, (0..2 * n).map(|i| if i % 3 == 0 { full(bits) } else { -(1 << (bits - 1)) }).collect()));
    v.push(("stereo extremes opposite".into(), 2, interleave(&vec![full(bits); n], &vec![-(1 << (bits - 1)); n])));
    // Wasted bits: every sample a multiple of 256 (and an odd-only channel).
    v.push(("wasted bits".into(), 1, sine(n, bits, 0.6, 9.0).iter().map(|&s| s & !255).collect()));
    v.push(("wasted bits stereo".into(), 2, interleave(&sine(n, bits, 0.6, 9.0).iter().map(|&s| s & !15).collect::<Vec<_>>(), &sine(n, bits, 0.3, 5.0).iter().map(|&s| s & !3).collect::<Vec<_>>())));
    // Lengths around the block size, one sample, a few.
    for len in [1usize, 2, 3, 15, 16, 17, 100, 4095, 4096, 4097, 8192, 8193, 12_289] {
        v.push((format!("sine, {len} frames mono"), 1, sine(len, bits, 0.9, (len as f64 / 50.0).max(1.0))));
        v.push((format!("noise, {len} frames stereo"), 2, (0..2 * len).map(|_| noise.sample(bits)).collect()));
    }
    v
}

#[test]
fn every_generated_signal_round_trips_bit_exactly() {
    for bits in [16u8, 24] {
        for (name, ch, x) in signals(bits) {
            round_trip(&x, ch, bits, 48_000, &format!("{name} at {bits} bits"));
        }
    }
}

#[test]
fn every_rate_and_both_depths_are_named_in_the_headers() {
    let x = interleave(&sine(9000, 16, 0.5, 20.0), &sine(9000, 16, 0.3, 31.0));
    for rate in [44_100, 48_000, 88_200, 96_000, 8_000, 22_050, 176_400, 192_000, 11_025, 12_345] {
        round_trip(&x, 2, 16, rate, &format!("{rate} Hz"));
    }
    let y = sine(9000, 24, 0.5, 20.0);
    for rate in [44_100, 48_000, 88_200, 96_000] {
        round_trip(&y, 1, 24, rate, &format!("{rate} Hz 24-bit"));
    }
}

#[test]
fn empty_input_is_a_valid_empty_file() {
    for ch in [1u16, 2] {
        let bytes = round_trip(&[], ch, 16, 44_100, "empty");
        assert_eq!(bytes.len(), 42);
    }
}

#[test]
fn the_encoder_picks_sensible_modes() {
    // Silence: constant subframes, a tiny file.
    let silent = encode(&vec![0; 8192], 1, 16, 48_000).unwrap();
    let d = decode(&silent.bytes).unwrap();
    assert!(d.modes.iter().all(|(_, t)| t == &vec![0u8]), "{:?}", d.modes);
    assert!(silent.bytes.len() < 42 + 2 * 20, "{}", silent.bytes.len());
    // Identical channels: left and side (a constant side), or mid and side.
    let l = sine(5000, 16, 0.7, 13.0);
    let d = decode(&encode(&interleave(&l, &l), 2, 16, 48_000).unwrap().bytes).unwrap();
    assert!(d.modes.iter().all(|(a, t)| *a != 1 && t.contains(&0)), "{:?}", d.modes);
    // A pure sine is predicted (a fixed predictor does well on it), not stored.
    let d = decode(&encode(&sine(5000, 16, 0.9, 40.0), 1, 16, 48_000).unwrap().bytes).unwrap();
    assert!(d.modes.iter().all(|(_, t)| t[0] >= 8), "{:?}", d.modes);
    // Two tones and a little noise: the fixed predictors amplify the noise,
    // LPC does not, so LPC wins.
    let mut n = Noise(3);
    let mix: Vec<i32> = sine(9000, 16, 0.4, 60.0).iter().zip(sine(9000, 16, 0.3, 177.0)).map(|(a, b)| a + b + (n.next() % 41) as i32 - 20).collect();
    let enc = encode(&mix, 1, 16, 48_000).unwrap();
    let d = decode(&enc.bytes).unwrap();
    assert!(d.modes.iter().all(|(_, t)| t[0] >= 32), "LPC is used: {:?}", d.modes);
    assert_eq!(d.samples, mix);
    // Full-scale noise cannot be predicted: verbatim, a little over the PCM size.
    let mut noise = Noise(5);
    let x: Vec<i32> = (0..8192).map(|_| noise.sample(16)).collect();
    let enc = encode(&x, 1, 16, 48_000).unwrap();
    let d = decode(&enc.bytes).unwrap();
    assert!(d.modes.iter().all(|(_, t)| t[0] == 1), "{:?}", d.modes);
    assert!(enc.bytes.len() < 42 + 8192 * 2 + 2 * 16, "{}", enc.bytes.len());
}

#[test]
fn bad_input_is_refused_and_damage_is_found() {
    assert_eq!(encode(&[0; 6], 3, 16, 48_000).unwrap_err(), FlacError::Channels(3));
    assert_eq!(encode(&[0; 6], 0, 16, 48_000).unwrap_err(), FlacError::Channels(0));
    assert_eq!(encode(&[0; 6], 2, 20, 48_000).unwrap_err(), FlacError::BitDepth(20));
    assert_eq!(encode(&[0; 6], 2, 16, 0).unwrap_err(), FlacError::SampleRate(0));
    assert_eq!(encode(&[0; 6], 2, 16, 700_000).unwrap_err(), FlacError::SampleRate(700_000));
    assert_eq!(encode(&[0; 5], 2, 16, 48_000).unwrap_err(), FlacError::Ragged);
    assert_eq!(encode(&[32768], 1, 16, 48_000).unwrap_err(), FlacError::OutOfRange);
    assert_eq!(encode(&[-32769], 1, 16, 48_000).unwrap_err(), FlacError::OutOfRange);
    assert!(encode(&[-32768, 32767], 1, 16, 48_000).is_ok());
    assert_eq!(encode(&[1 << 23], 1, 24, 48_000).unwrap_err(), FlacError::OutOfRange);
    // The decoder is no rubber stamp: any flipped bit in the frames is caught.
    let x = interleave(&sine(6000, 16, 0.8, 30.0), &sine(6000, 16, 0.5, 17.0));
    let good = encode(&x, 2, 16, 44_100).unwrap().bytes;
    assert!(decode(&good).is_ok());
    for at in (42..good.len()).step_by(97) {
        let mut bad = good.clone();
        bad[at] ^= 0x10;
        assert!(decode(&bad).is_err(), "a flipped bit at byte {at} went unnoticed");
    }
}

#[test]
fn floats_become_integers_as_the_16_bit_wav_writer_does_it() {
    let mut noise = Noise(11);
    let x: Vec<f32> = (0..50_000).map(|_| (noise.next() % 2_000_001) as f32 / 1_000_000.0 - 1.0).chain([0.0, 1.0, -1.0, 0.5, -0.5, 1.5, -2.0, f32::NAN]).collect();
    let pcm = float_to_pcm(&x, 16);
    let wav = crate::wav::encode(&x, 1, 48_000, false);
    let from_wav: Vec<i32> = wav[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as i32).collect();
    // The WAV writer sends NaN through `clamp` and the cast: 0 as well.
    assert_eq!(pcm.samples, from_wav, "same integers as wav::encode");
    let over = x.iter().filter(|s| s.abs() > 1.0 || s.is_nan()).count();
    assert_eq!(pcm.clipped, over);
    assert_eq!(over, 3, "1.5, -2.0 and NaN");
    // 24 bits: full scale is +-8388607, halves round away from zero, no dither.
    let p = float_to_pcm(&[1.0, -1.0, 0.0, 0.5, -0.5, 2.0, 1e-9], 24);
    assert_eq!(p.samples, vec![8_388_607, -8_388_607, 0, 4_194_304, -4_194_304, 8_388_607, 0]);
    assert_eq!(p.clipped, 1);
    // Exactly full scale is not a clip. Deterministic: the same input, the same output.
    assert_eq!(float_to_pcm(&[1.0, -1.0], 16).clipped, 0);
    assert_eq!(float_to_pcm(&x, 24), float_to_pcm(&x, 24));
    // A round trip through FLAC returns exactly those integers.
    let (enc, clipped) = encode_f32(&x, 1, 24, 48_000).unwrap();
    assert_eq!(clipped, 3);
    assert_eq!(decode(&enc.bytes).unwrap().samples, float_to_pcm(&x, 24).samples);
}

/// Sizes of the generated signals (run with `--nocapture` to see them).
#[test]
fn compression_ratios_of_the_generated_signals() {
    for bits in [16u8, 24] {
        println!("--- {bits}-bit, 48 kHz: signal, PCM bytes, FLAC bytes, FLAC/PCM");
        for (name, ch, x) in signals(bits).into_iter().filter(|s| !s.0.contains("frames")) {
            let enc = encode(&x, ch, bits, 48_000).unwrap();
            let pcm = x.len() * bits as usize / 8;
            println!("{name:32} {pcm:9} {:9} {:6.3}", enc.bytes.len(), enc.bytes.len() as f64 / pcm as f64);
        }
    }
}

// --- The system's own decoders as an independent witness --------------------

fn run(cmd: &str, args: &[&str]) -> (String, String) {
    let out = std::process::Command::new(cmd).args(args).output().unwrap_or_else(|e| panic!("cannot run {cmd}: {e}"));
    assert!(out.status.success() || cmd == "afinfo", "{cmd} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The integers of a PCM WAV a system tool wrote.
fn wav_ints(path: &std::path::Path, bits: u8) -> Vec<i32> {
    let bytes = std::fs::read(path).unwrap();
    let (d, _) = crate::wav::decode(&bytes).unwrap();
    let scale = (1u64 << (bits - 1)) as f32;
    d.samples.iter().map(|&s| (s * scale).round() as i32).collect()
}

/// Run by hand: `cargo test --release -p echlub-audiofile -- --ignored
/// --nocapture external_decoders`. Needs `ffmpeg` (ECHLUB_FFMPEG, else
/// Homebrew's default location or the PATH) and macOS's `afconvert` and
/// `afinfo`. Each generated file is decoded by ffmpeg (errors read, MD5 of
/// the decoded samples compared with STREAMINFO's, samples compared with
/// the source) and by afconvert (samples compared with the source).
#[test]
#[ignore = "needs ffmpeg and macOS afconvert; run by hand"]
fn external_decoders_agree_sample_for_sample() {
    let ffmpeg = std::env::var("ECHLUB_FFMPEG").unwrap_or_else(|_| if std::path::Path::new("/opt/homebrew/bin/ffmpeg").exists() { "/opt/homebrew/bin/ffmpeg".into() } else { "ffmpeg".into() });
    let dir = std::env::temp_dir().join(format!("echlub-flac-witness-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = 0;
    for bits in [16u8, 24] {
        for (name, ch, x) in signals(bits) {
            let rate = [44_100, 48_000, 88_200, 96_000][files % 4];
            let enc = encode(&x, ch, bits, rate).unwrap();
            let tag: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
            let flac = dir.join(format!("{tag}-{bits}.flac"));
            std::fs::write(&flac, &enc.bytes).unwrap();
            let f = flac.to_str().unwrap();
            // ffmpeg: no decode errors, ...
            let (_, err) = run(&ffmpeg, &["-v", "error", "-i", f, "-f", "null", "-"]);
            assert!(err.trim().is_empty(), "{name} {bits}: ffmpeg reports: {err}");
            // ... the MD5 of what it decodes is the one in STREAMINFO, ...
            let codec = if bits == 16 { "pcm_s16le" } else { "pcm_s24le" };
            let (md5_out, _) = run(&ffmpeg, &["-v", "error", "-i", f, "-map", "0:a", "-c:a", codec, "-f", "md5", "-"]);
            assert_eq!(md5_out.trim(), format!("MD5={}", crate::md5::hex(&enc.md5)), "{name} {bits}");
            // ... and its samples are the source's.
            let out = dir.join(format!("{tag}-{bits}-ffmpeg.wav"));
            run(&ffmpeg, &["-y", "-v", "error", "-i", f, "-c:a", codec, out.to_str().unwrap()]);
            assert!(wav_ints(&out, bits) == x, "{name} {bits}: ffmpeg's samples differ");
            // afconvert.
            let out = dir.join(format!("{tag}-{bits}-afconvert.wav"));
            let fmt = if bits == 16 { "LEI16" } else { "LEI24" };
            run("afconvert", &["-f", "WAVE", "-d", fmt, f, out.to_str().unwrap()]);
            assert!(wav_ints(&out, bits) == x, "{name} {bits}: afconvert's samples differ");
            files += 1;
        }
    }
    let (info, _) = run("afinfo", &[dir.join("stereo_sines-16.flac").to_str().unwrap()]);
    println!("afinfo:\n{info}");
    println!("{files} files, each decoded by ffmpeg and by afconvert, equal to the source sample for sample, ffmpeg's MD5 equal to STREAMINFO's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run by hand on a rendered mix: `ECHLUB_FLAC_DEMO_WAV=<wav> cargo test
/// --release -p echlub-audiofile -- --ignored --nocapture demo_mix`. The
/// WAV is only read. Encodes it at 16 and 24 bits, times the encoder, checks
/// the round trip with the test decoder, and prints the sizes. With
/// ECHLUB_FLAC_KEEP=<dir> the FLAC files are kept there (to check them with
/// ffmpeg by hand).
#[test]
#[ignore = "needs ECHLUB_FLAC_DEMO_WAV; run by hand"]
fn demo_mix_ratio_and_round_trip() {
    let path = std::env::var("ECHLUB_FLAC_DEMO_WAV").expect("ECHLUB_FLAC_DEMO_WAV");
    let bytes = std::fs::read(&path).unwrap();
    let (d, rate) = crate::wav::decode(&bytes).unwrap();
    println!("{path}: {} Hz, {} ch, {:.1} s, {} bytes as WAV", rate, d.channels, d.frames() as f64 / rate as f64, bytes.len());
    for bits in [16u8, 24] {
        let t = std::time::Instant::now();
        let (enc, clipped) = encode_f32(&d.samples, d.channels, bits, rate).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let pcm = float_to_pcm(&d.samples, bits);
        let dec = decode(&enc.bytes).unwrap();
        assert!(dec.samples == pcm.samples, "round trip");
        let raw = pcm.samples.len() * bits as usize / 8;
        println!("{bits}-bit: PCM {raw} bytes, FLAC {} bytes, ratio {:.3}, {clipped} clipped, encoded in {secs:.2} s ({:.0}x real time)", enc.bytes.len(), enc.bytes.len() as f64 / raw as f64, d.frames() as f64 / rate as f64 / secs);
        if let Ok(dir) = std::env::var("ECHLUB_FLAC_KEEP") {
            std::fs::write(std::path::Path::new(&dir).join(format!("demo-{bits}.flac")), &enc.bytes).unwrap();
        }
    }
}
