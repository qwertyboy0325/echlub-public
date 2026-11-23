//! A FLAC encoder (lossless, 16- or 24-bit, mono or stereo), on plain
//! sample buffers: no dependency, platform-neutral, for control and worker
//! threads (it allocates; never the audio thread).
//!
//! What it writes: `fLaC`, one STREAMINFO block (with the MD5 of the
//! unencoded audio, [`crate::md5`]), then frames of a fixed 4096 samples
//! (the last one shorter) each with its CRC-8 header and CRC-16. Every
//! subframe is the cheapest of constant, verbatim, fixed predictor 0..=4
//! and LPC (orders up to 12, quantised coefficients, Tukey window), with
//! Rice-coded residuals in the best partitioning (4- or 5-bit parameters).
//! Stereo is the cheapest of left/right, left/side, side/right and
//! mid/side. Wasted bits (low bits all zero in a subframe) are dropped.
//! Not written: seek table, other metadata, escape-coded Rice partitions,
//! variable block size, more than two channels, other bit depths.
//!
//! The tests hold a decoder for exactly this subset and prove bit-exact
//! round trips; an ignored test checks the files with the system's own
//! decoders.

use crate::md5::Md5;

/// Samples per channel in a frame (the last frame may be shorter).
pub const BLOCK_SIZE: usize = 4096;
const MAX_LPC_ORDER: usize = 12;
const MAX_PARTITION_ORDER: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlacError {
    /// Only 1 or 2 channels.
    Channels(u16),
    /// Only 16 or 24 bits.
    BitDepth(u8),
    /// 1 to 655 350 Hz (what a frame header can name).
    SampleRate(u32),
    /// The sample count is not a whole number of frames.
    Ragged,
    /// A sample outside the bit depth's range.
    OutOfRange,
}

impl core::fmt::Display for FlacError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FlacError::Channels(c) => write!(f, "FLAC takes 1 or 2 channels here, not {c}"),
            FlacError::BitDepth(b) => write!(f, "FLAC takes 16 or 24 bits here, not {b}"),
            FlacError::SampleRate(r) => write!(f, "FLAC cannot name a sample rate of {r} Hz"),
            FlacError::Ragged => write!(f, "the samples are not a whole number of frames"),
            FlacError::OutOfRange => write!(f, "a sample is outside the bit depth's range"),
        }
    }
}

/// An encoded file and the MD5 of the audio it holds (as in STREAMINFO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub md5: [u8; 16],
}

/// Float samples as integers of `bits` (16 or 24): the same conversion as
/// the 16-bit WAV writer (`wav::encode`): clamp to -1..=1, scale by
/// `2^(bits-1) - 1`, round half away from zero. No dither. Samples beyond
/// full scale (and NaN, which becomes 0) are counted in `clipped`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub samples: Vec<i32>,
    pub clipped: usize,
}

pub fn float_to_pcm(samples: &[f32], bits: u8) -> Pcm {
    let mut clipped = 0;
    let out = samples
        .iter()
        .map(|&s| {
            if s.abs() > 1.0 || s.is_nan() {
                clipped += 1;
            }
            if bits <= 16 {
                (s.clamp(-1.0, 1.0) * 32767.0).round() as i32
            } else {
                ((s as f64).clamp(-1.0, 1.0) * 8_388_607.0).round() as i32
            }
        })
        .collect();
    Pcm { samples: out, clipped }
}

/// Interleaved float samples as a FLAC file of `bits` (16 or 24); the
/// count of samples that were beyond full scale comes back with it.
pub fn encode_f32(samples: &[f32], channels: u16, bits: u8, sample_rate: u32) -> Result<(Encoded, usize), FlacError> {
    let pcm = float_to_pcm(samples, bits);
    Ok((encode(&pcm.samples, channels, bits, sample_rate)?, pcm.clipped))
}

const CRC8: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

const CRC16: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| CRC8[(c ^ b) as usize])
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |c, &b| (c << 8) ^ CRC16[((c >> 8) as u8 ^ b) as usize])
}

/// MSB-first bit writer.
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn new() -> Bits {
        Bits { out: Vec::new(), acc: 0, n: 0 }
    }
    /// The low `bits` (at most 32) of `v`.
    fn put(&mut self, v: u64, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (v & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }
    fn put_signed(&mut self, v: i64, bits: u32) {
        self.put(v as u64, bits);
    }
    fn zeros(&mut self, mut count: u64) {
        while count > 0 {
            let c = count.min(32);
            self.put(0, c as u32);
            count -= c;
        }
    }
    fn pad(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

/// One Rice-coded residual: the method (4- or 5-bit parameters), the
/// partition order, a parameter per partition and the exact bit count
/// (method, order and parameters included).
#[derive(Debug, Clone)]
struct Rice {
    method: u32,
    order: u32,
    params: Vec<u32>,
    bits: u64,
}

fn zigzag(r: i32) -> u32 {
    ((r as u32) << 1) ^ ((r >> 31) as u32)
}

/// The best parameter (at most `kmax`) for one partition and its bit cost.
fn part_cost(u: &[u32], kmax: u32) -> (u32, u64) {
    let sum: u64 = u.iter().map(|&v| v as u64).sum();
    let m = u.len() as u64;
    let est = (if sum / m == 0 { 0 } else { 63 - (sum / m).leading_zeros() }).min(kmax);
    let mut best = (0, u64::MAX);
    for k in est.saturating_sub(1)..=(est + 1).min(kmax) {
        let bits = m * (k as u64 + 1) + u.iter().map(|&v| (v >> k) as u64).sum::<u64>();
        if bits < best.1 {
            best = (k, bits);
        }
    }
    best
}

/// The cheapest partitioning of `res` (the residual of a block of `n`
/// samples after `order` warm-up samples).
fn best_rice(res: &[i32], n: usize, order: usize) -> Rice {
    let u: Vec<u32> = res.iter().map(|&r| zigzag(r)).collect();
    let mut max_p = 0;
    while max_p < MAX_PARTITION_ORDER && n.is_multiple_of(1 << (max_p + 1)) && (n >> (max_p + 1)) > order {
        max_p += 1;
    }
    let big = u.iter().any(|&v| v >= 1 << 17);
    let mut best: Option<Rice> = None;
    for (method, kmax, width) in [(0u32, 14u32, 4u64), (1, 30, 5)] {
        if method == 1 && !big {
            continue;
        }
        for p in 0..=max_p {
            let per = n >> p;
            let (mut params, mut bits, mut at) = (Vec::new(), 2 + 4u64, 0usize);
            for i in 0..(1usize << p) {
                let len = if i == 0 { per - order } else { per };
                let (k, b) = part_cost(&u[at..at + len], kmax);
                at += len;
                params.push(k);
                bits += width + b;
            }
            if best.as_ref().is_none_or(|b| bits < b.bits) {
                best = Some(Rice { method, order: p, params, bits });
            }
        }
    }
    best.expect("at least partition order 0")
}

fn write_rice(w: &mut Bits, r: &Rice, res: &[i32], n: usize, order: usize) {
    w.put(r.method as u64, 2);
    w.put(r.order as u64, 4);
    let width = if r.method == 0 { 4 } else { 5 };
    let per = n >> r.order;
    let mut at = 0;
    for (i, &k) in r.params.iter().enumerate() {
        w.put(k as u64, width);
        let len = if i == 0 { per - order } else { per };
        for &v in &res[at..at + len] {
            let u = zigzag(v) as u64;
            w.zeros(u >> k);
            w.put(1, 1);
            w.put(u & ((1u64 << k) - 1), k);
        }
        at += len;
    }
}

#[derive(Debug, Clone)]
enum Kind {
    Constant,
    Verbatim,
    Fixed { order: usize },
    Lpc { order: usize, precision: u32, shift: u32, coefs: Vec<i32> },
}

/// How one subframe will be written.
#[derive(Debug, Clone)]
struct Plan {
    /// The samples as coded (wasted bits shifted off).
    y: Vec<i32>,
    wasted: u32,
    bps: u32,
    kind: Kind,
    residual: Vec<i32>,
    rice: Option<Rice>,
    bits: u64,
}

fn fixed_residual(y: &[i32], order: usize) -> Option<Vec<i32>> {
    const C: [[i64; 5]; 5] = [[1, 0, 0, 0, 0], [1, -1, 0, 0, 0], [1, -2, 1, 0, 0], [1, -3, 3, -1, 0], [1, -4, 6, -4, 1]];
    let c = C[order];
    let mut out = Vec::with_capacity(y.len() - order);
    for i in order..y.len() {
        let mut s = 0i64;
        for (j, &cj) in c.iter().enumerate().take(order + 1) {
            s += cj * y[i - j] as i64;
        }
        out.push(i32::try_from(s).ok()?);
    }
    Some(out)
}

fn lpc_residual(y: &[i32], coefs: &[i32], shift: u32) -> Option<Vec<i32>> {
    let order = coefs.len();
    let mut out = Vec::with_capacity(y.len() - order);
    for i in order..y.len() {
        let mut s = 0i64;
        for (j, &c) in coefs.iter().enumerate() {
            s += c as i64 * y[i - 1 - j] as i64;
        }
        out.push(i32::try_from(y[i] as i64 - (s >> shift)).ok()?);
    }
    Some(out)
}

/// Linear predictors of every order up to `max_order` from the
/// autocorrelation of the windowed block (Levinson-Durbin): the
/// coefficients (prediction = sum a[j] * x[t-1-j]) and the prediction
/// error of each order.
fn lpc_orders(y: &[i32], max_order: usize) -> Option<Vec<(Vec<f64>, f64)>> {
    let n = y.len();
    let np = (n / 4).max(1);
    let w: Vec<f64> = (0..n)
        .map(|i| {
            let d = i.min(n - 1 - i);
            if d < np {
                0.5 - 0.5 * (core::f64::consts::PI * d as f64 / np as f64).cos()
            } else {
                1.0
            }
        })
        .collect();
    let x: Vec<f64> = y.iter().zip(&w).map(|(&s, w)| s as f64 * w).collect();
    let mut r = vec![0.0f64; max_order + 1];
    for (lag, r) in r.iter_mut().enumerate() {
        *r = x[lag..].iter().zip(&x).map(|(a, b)| a * b).sum();
    }
    if r[0] <= 0.0 {
        return None;
    }
    r[0] *= 1.0 + 1e-9;
    let mut a: Vec<f64> = Vec::new();
    let mut err = r[0];
    let mut out = Vec::new();
    for m in 1..=max_order {
        let acc: f64 = (0..m - 1).map(|j| a[j] * r[m - 1 - j]).sum();
        let k = (r[m] - acc) / err;
        let prev = a.clone();
        a.push(k);
        for j in 0..m - 1 {
            a[j] = prev[j] - k * prev[m - 2 - j];
        }
        err *= 1.0 - k * k;
        if !(err.is_finite() && err > 0.0) {
            break;
        }
        out.push((a.clone(), err));
    }
    (!out.is_empty()).then_some(out)
}

/// Quantise `a` to `precision` bits (sign included) with a shift
/// (0..=15), feeding the rounding error forward. `None` when the
/// coefficients are too large for any shift.
fn quantize(a: &[f64], precision: u32) -> Option<(Vec<i32>, u32)> {
    let cmax = a.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if !(cmax > 0.0 && cmax.is_finite()) {
        return None;
    }
    let e = (cmax.log2().floor() as i32) + 1;
    let shift = precision as i32 - 1 - e;
    if shift < 0 {
        return None;
    }
    let shift = shift.min(15) as u32;
    let (qmax, qmin) = ((1i64 << (precision - 1)) - 1, -(1i64 << (precision - 1)));
    let scale = (1u64 << shift) as f64;
    let mut err = 0.0;
    let q = a
        .iter()
        .map(|&c| {
            let v = c * scale + err;
            let q = (v.round() as i64).clamp(qmin, qmax);
            err = v - q as f64;
            q as i32
        })
        .collect();
    Some((q, shift))
}

fn plan_subframe(x: &[i32], bps: u32) -> Plan {
    let n = x.len();
    if x.iter().all(|&v| v == x[0]) {
        return Plan { y: x.to_vec(), wasted: 0, bps, kind: Kind::Constant, residual: Vec::new(), rice: None, bits: 8 + bps as u64 };
    }
    let or = x.iter().fold(0i32, |a, &v| a | v);
    let wasted = or.trailing_zeros().min(bps - 1);
    let y: Vec<i32> = x.iter().map(|&v| v >> wasted).collect();
    let bps = bps - wasted;
    let head = 8 + wasted as u64;
    let mut best = Plan { y: y.clone(), wasted, bps, kind: Kind::Verbatim, residual: Vec::new(), rice: None, bits: head + n as u64 * bps as u64 };
    for order in 0..=4usize.min(n - 1) {
        let Some(res) = fixed_residual(&y, order) else { continue };
        let rice = best_rice(&res, n, order);
        let bits = head + order as u64 * bps as u64 + rice.bits;
        if bits < best.bits {
            best = Plan { y: y.clone(), wasted, bps, kind: Kind::Fixed { order }, residual: res, rice: Some(rice), bits };
        }
    }
    let max_order = MAX_LPC_ORDER.min(n / 4);
    if n >= 32 && max_order >= 1 {
        if let Some(orders) = lpc_orders(&y, max_order) {
            let precision: u32 = match n {
                0..=192 => 7,
                193..=384 => 8,
                385..=576 => 9,
                577..=1152 => 10,
                1153..=2304 => 11,
                2305..=4608 => 12,
                _ => 13,
            };
            // The two orders the error estimate likes best.
            let mut est: Vec<(f64, usize)> = orders
                .iter()
                .enumerate()
                .map(|(i, (_, err))| {
                    let m = i + 1;
                    let per = (0.5 * ((0.5 / n as f64) * err).log2()).max(0.0);
                    (per * (n - m) as f64 + m as f64 * (bps + precision) as f64, m)
                })
                .collect();
            est.sort_by(|a, b| a.0.total_cmp(&b.0));
            for &(_, m) in est.iter().take(2) {
                let Some((coefs, shift)) = quantize(&orders[m - 1].0, precision) else { continue };
                let Some(res) = lpc_residual(&y, &coefs, shift) else { continue };
                let rice = best_rice(&res, n, m);
                let bits = head + m as u64 * bps as u64 + 4 + 5 + m as u64 * precision as u64 + rice.bits;
                if bits < best.bits {
                    best = Plan { y: y.clone(), wasted, bps, kind: Kind::Lpc { order: m, precision, shift, coefs }, residual: res, rice: Some(rice), bits };
                }
            }
        }
    }
    best
}

fn write_subframe(w: &mut Bits, p: &Plan) {
    let n = p.y.len();
    let ty: u64 = match p.kind {
        Kind::Constant => 0,
        Kind::Verbatim => 1,
        Kind::Fixed { order } => 8 | order as u64,
        Kind::Lpc { order, .. } => 32 | (order as u64 - 1),
    };
    w.put(0, 1);
    w.put(ty, 6);
    if p.wasted > 0 {
        w.put(1, 1);
        w.zeros(p.wasted as u64 - 1);
        w.put(1, 1);
    } else {
        w.put(0, 1);
    }
    match &p.kind {
        Kind::Constant => w.put_signed(p.y[0] as i64, p.bps),
        Kind::Verbatim => p.y.iter().for_each(|&v| w.put_signed(v as i64, p.bps)),
        Kind::Fixed { order } => {
            p.y[..*order].iter().for_each(|&v| w.put_signed(v as i64, p.bps));
            write_rice(w, p.rice.as_ref().expect("rice"), &p.residual, n, *order);
        }
        Kind::Lpc { order, precision, shift, coefs } => {
            p.y[..*order].iter().for_each(|&v| w.put_signed(v as i64, p.bps));
            w.put(*precision as u64 - 1, 4);
            w.put(*shift as u64, 5);
            coefs.iter().for_each(|&c| w.put_signed(c as i64, *precision));
            write_rice(w, p.rice.as_ref().expect("rice"), &p.residual, n, *order);
        }
    }
}

/// A frame number as the UTF-8-style code of the frame header.
fn utf8_number(n: u64) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let extra = match n {
        0..=0x7ff => 1,
        0x800..=0xffff => 2,
        0x1_0000..=0x1f_ffff => 3,
        0x20_0000..=0x3ff_ffff => 4,
        _ => 5,
    };
    let lead = 0xffu8 << (7 - extra) as u32;
    let mut out = vec![lead | (n >> (6 * extra)) as u8];
    for i in (0..extra).rev() {
        out.push(0x80 | ((n >> (6 * i)) & 0x3f) as u8);
    }
    out
}

fn rate_code(rate: u32) -> u8 {
    match rate {
        88_200 => 1,
        176_400 => 2,
        192_000 => 3,
        8_000 => 4,
        16_000 => 5,
        22_050 => 6,
        24_000 => 7,
        32_000 => 8,
        44_100 => 9,
        48_000 => 10,
        96_000 => 11,
        _ => 0,
    }
}

fn frame(index: u64, channels: &[Vec<i32>], bits: u32, rate: u32) -> Vec<u8> {
    let n = channels[0].len();
    // Which channel set to code.
    let (assignment, coded): (u8, Vec<Plan>) = if channels.len() == 1 {
        (0, vec![plan_subframe(&channels[0], bits)])
    } else {
        let (l, r) = (&channels[0], &channels[1]);
        let mid: Vec<i32> = l.iter().zip(r).map(|(&a, &b)| ((a as i64 + b as i64) >> 1) as i32).collect();
        let side: Vec<i32> = l.iter().zip(r).map(|(&a, &b)| (a as i64 - b as i64) as i32).collect();
        let (pl, pr) = (plan_subframe(l, bits), plan_subframe(r, bits));
        let (pm, ps) = (plan_subframe(&mid, bits), plan_subframe(&side, bits + 1));
        let options = [(pl.bits + pr.bits, 1u8), (pl.bits + ps.bits, 8), (ps.bits + pr.bits, 9), (pm.bits + ps.bits, 10)];
        let (_, pick) = options.iter().copied().min_by_key(|o| o.0).expect("four options");
        let coded = match pick {
            1 => vec![pl, pr],
            8 => vec![pl, ps],
            9 => vec![ps, pr],
            _ => vec![pm, ps],
        };
        (pick, coded)
    };
    let mut head = vec![0xff, 0xf8];
    let size_code: u8 = match n {
        192 => 1,
        576 => 2,
        1152 => 3,
        2304 => 4,
        4608 => 5,
        256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 => 8 + (n / 256).trailing_zeros() as u8,
        _ if n <= 256 => 6,
        _ => 7,
    };
    head.push(size_code << 4 | rate_code(rate));
    let size_bits: u8 = if bits == 16 { 0b100 } else { 0b110 };
    head.push(assignment << 4 | size_bits << 1);
    head.extend(utf8_number(index));
    match size_code {
        6 => head.push((n - 1) as u8),
        7 => head.extend_from_slice(&((n - 1) as u16).to_be_bytes()),
        _ => {}
    }
    head.push(crc8(&head));
    let mut w = Bits::new();
    for p in &coded {
        write_subframe(&mut w, p);
    }
    w.pad();
    let mut out = head;
    out.extend(w.out);
    let c = crc16(&out);
    out.extend_from_slice(&c.to_be_bytes());
    out
}

/// Interleaved integer samples (`channels` 1 or 2, `bits` 16 or 24, each
/// sample within the bit depth's signed range) as a FLAC file.
pub fn encode(samples: &[i32], channels: u16, bits: u8, sample_rate: u32) -> Result<Encoded, FlacError> {
    if !(1..=2).contains(&channels) {
        return Err(FlacError::Channels(channels));
    }
    if bits != 16 && bits != 24 {
        return Err(FlacError::BitDepth(bits));
    }
    if sample_rate == 0 || sample_rate > 655_350 {
        return Err(FlacError::SampleRate(sample_rate));
    }
    let ch = channels as usize;
    if !samples.len().is_multiple_of(ch) {
        return Err(FlacError::Ragged);
    }
    let (lo, hi) = (-(1i32 << (bits - 1)), (1i32 << (bits - 1)) - 1);
    if samples.iter().any(|&s| s < lo || s > hi) {
        return Err(FlacError::OutOfRange);
    }
    let frames = samples.len() / ch;
    // STREAMINFO always says 4096 (a shorter stream is one short last frame: the
    // system's own decoder, afconvert, refuses a file whose STREAMINFO says less).
    let block = BLOCK_SIZE;
    let mut md5 = Md5::new();
    let bytes_per = bits as usize / 8;
    let mut body = Vec::new();
    let (mut min_frame, mut max_frame) = (usize::MAX, 0usize);
    let mut start = 0;
    let mut index = 0u64;
    while start < frames {
        let n = (frames - start).min(block);
        let part = &samples[start * ch..(start + n) * ch];
        let mut raw = Vec::with_capacity(part.len() * bytes_per);
        for &s in part {
            raw.extend_from_slice(&s.to_le_bytes()[..bytes_per]);
        }
        md5.update(&raw);
        let planes: Vec<Vec<i32>> = (0..ch).map(|c| part.iter().skip(c).step_by(ch).copied().collect()).collect();
        let f = frame(index, &planes, bits as u32, sample_rate);
        min_frame = min_frame.min(f.len());
        max_frame = max_frame.max(f.len());
        body.extend(f);
        start += n;
        index += 1;
    }
    if index == 0 {
        min_frame = 0;
    }
    let digest = md5.finish();
    let mut out = Vec::with_capacity(42 + body.len());
    out.extend_from_slice(b"fLaC");
    out.extend_from_slice(&[0x80, 0, 0, 34]);
    out.extend_from_slice(&(block as u16).to_be_bytes());
    out.extend_from_slice(&(block as u16).to_be_bytes());
    out.extend_from_slice(&(min_frame as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&(max_frame as u32).to_be_bytes()[1..]);
    // 20 bits rate, 3 bits channels - 1, 5 bits depth - 1, 36 bits total samples.
    let packed: u64 = (sample_rate as u64) << 44 | (ch as u64 - 1) << 41 | (bits as u64 - 1) << 36 | frames as u64;
    out.extend_from_slice(&packed.to_be_bytes());
    out.extend_from_slice(&digest);
    out.extend(body);
    Ok(Encoded { bytes: out, md5: digest })
}

#[cfg(test)]
#[path = "flac_tests.rs"]
mod tests;
