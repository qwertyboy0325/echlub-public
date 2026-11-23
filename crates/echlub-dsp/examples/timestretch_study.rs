//! Time-stretching, stage 0: the measurements behind the choice of a
//! time-stretch algorithm.
//!
//! **What this is.** A research prototype, kept for the numbers it prints. It
//! implements four ways of stretching audio without changing its pitch (and of
//! shifting pitch without changing the length) and compares them on generated
//! signals. It is not real-time code and it is not what the EchLub engine runs:
//! everything is `f64`, unoptimised, allocates freely and works on whole
//! signals at once. The engine's own stretcher is a separate, streaming
//! implementation that is not in this repository; its cost figures come from
//! other benches, not from this file.
//!
//! **How to run it.** From the repository root:
//!
//! ```text
//! cargo run --release -p echlub-dsp --example timestretch_study
//! ```
//!
//! The full run takes about a minute and a quarter on an Apple M4 Max (one
//! observed run: 78 s). With `TS_NULL=1` in the environment it runs only the
//! two quick checks at the end of the report (the ratio-1 null by region and
//! a held tone under noise hits; under a second).
//!
//! **What it prints.** Plain-text tables, one row per stretch ratio (x0.5 to
//! x2) or pitch shift (-12 to +12 semitones) and one column per algorithm
//! (WSOLA, PV, WSOLA+T, PV+T), in this order: pitch error of a held tone and
//! of a chord (cents); modulation the source does not have on a steady chord
//! (dB); click smear and pre-echo; drum-hit attack time and pre-echo; onset
//! timing error (frames); inter-channel phase error (degrees); the largest
//! difference from the source at ratio 1; the cost of each algorithm in this
//! harness (ns per output frame; an upper bound, not an engine figure). Each
//! measure is defined where it is computed below. Nothing is asserted: the
//! example reports, it does not pass or fail.
//!
//! Prototypes of the candidates, offline over whole generated signals
//! (f64, unoptimised, allocating: for comparison, not for the engine):
//!
//! - **WSOLA**: Hann frames of 1024, synthesis hop 512, the next frame
//!   taken where it best continues the last (normalised cross-correlation
//!   of the mid channel over the 512-frame overlap, within +-256 frames of
//!   its nominal place: coarse step 4, then refined), the same offset for
//!   both channels.
//! - **PV**: a phase vocoder, Hann frames of 4096, synthesis hop 1024
//!   (a frame of 2048 measured too, for the chord), with identity phase
//!   locking (Laroche and Dolson): peaks of the mid channel's magnitude
//!   carry the phase advance, every bin of a peak's region turns with it.
//!   Both channels turn by the mid's rotation, so the phase between them
//!   is the source's.
//! - **WSOLA+T**: transients passed through: onsets found in the source;
//!   the time map has slope 1 from 5 ms before to 40 ms after each, the
//!   onset at exactly t * ratio, the stretch taken between; in that zone
//!   the output is the source itself, crossfaded over 2 ms at each edge.
//! - **PV+T**: the same time map, and the transient kept inside the
//!   vocoder (copying the zone from the source, as WSOLA+T does, breaks
//!   any tone that goes on under the hit: its phase is the vocoder's on
//!   one side and the source's on the other):
//!   - the gates: a frame centred before an onset (1 ms before it) does
//!     not see past it; a frame centred after it that sees it adds
//!     nothing before it in the output; one more frame is centred exactly
//!     there (with a ratio above 1 the hop's frames cannot reach it);
//!   - in the zone, the bins whose energy rose more than 6 dB over a
//!     whole window ending at the zone's start (12 dB near a tone of that
//!     window) take the source's own phases; every other bin goes on with
//!     the vocoder's, so tones carry on under the hit.
//!
//! A pitch shift of s semitones: stretch by ratio * 2^(s/12), then
//! resample by 2^(s/12) (a 32-tap Kaiser windowed sinc, the kernel family of the EchLub sampler).
//!
//! Measures (each defined where computed below): pitch of a held tone and
//! of a chord (cents), transient smear and pre-echo of a click and of a
//! drum hit, the attack time of the drum hit, modulation the source does
//! not have on a steady chord (roughness / phasiness), inter-channel
//! phase, identity at ratio 1, onset timing, cost.
//!
//!   nice -n 10 cargo run --release -p echlub-dsp --example timestretch_study
use std::time::Instant;

use echlub_dsp::math;

const SR: f64 = 48_000.0;
const TAU: f64 = std::f64::consts::TAU;

type Stereo = (Vec<f64>, Vec<f64>);

// ---------------------------------------------------------------- FFT

/// A radix-2 complex FFT (ours: echlub_dsp math for the twiddles).
struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two());
        let (cos, sin) = (0..n / 2).map(|k| math::sin_cos(TAU * k as f64 / n as f64)).map(|(s, c)| (c, s)).unzip();
        Fft { n, cos, sin }
    }

    /// In place; `inverse` without the 1/n.
    fn run(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.n;
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (c, s) = (self.cos[k * step], if inverse { self.sin[k * step] } else { -self.sin[k * step] });
                    let (a, b) = (start + k, start + k + len / 2);
                    let (xr, xi) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
            }
            len <<= 1;
        }
    }
}

fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * math::cos(TAU * i as f64 / n as f64)).collect()
}

// ------------------------------------------------------------ time maps

/// Output time -> source time (frames), piecewise linear.
#[derive(Clone)]
struct Map {
    /// (output, source), rising in both.
    pts: Vec<(f64, f64)>,
}

impl Map {
    fn uniform(ratio: f64) -> Map {
        Map { pts: vec![(0.0, 0.0), (1e9, 1e9 / ratio)] }
    }

    /// Slope 1 from 5 ms before to 40 ms after each onset, the onset at
    /// t * ratio; the stretch between. A window that would run backwards
    /// (onsets too close for the ratio) is skipped.
    fn transients(ratio: f64, onsets: &[f64]) -> Map {
        let (pre, post) = (0.005 * SR, 0.040 * SR);
        let mut pts = vec![(0.0, 0.0)];
        for &o in onsets {
            let (y0, x0) = (o * ratio - pre, o - pre);
            let (y1, x1) = (o * ratio + post, o + post);
            let last = *pts.last().unwrap();
            if y0 > last.0 + 1.0 && x0 > last.1 + 1.0 {
                pts.push((y0, x0));
                pts.push((y1, x1));
            }
        }
        let last = *pts.last().unwrap();
        pts.push((last.0 + 1e9, last.1 + 1e9 / ratio));
        Map { pts }
    }

    fn src(&self, y: f64) -> f64 {
        let i = self.pts.partition_point(|p| p.0 <= y).clamp(1, self.pts.len() - 1);
        let (a, b) = (self.pts[i - 1], self.pts[i]);
        a.1 + (y - a.0) * (b.1 - a.1) / (b.0 - a.0)
    }
}

/// Onsets (source frames) of a stereo signal: the energy of the mid's
/// first difference (high frequencies weighted up, about 6 dB per octave:
/// a hit over a sustained tone stands out) in 2 ms blocks, rising 9 dB
/// above the mean of the 20 ms before and above -45 dB of the loudest
/// block; at most one per 30 ms. The onset is the first sample of that
/// block whose mid reaches a fifth of the block's peak.
fn onsets(x: &Stereo) -> Vec<f64> {
    let b = (0.002 * SR) as usize;
    let mid: Vec<f64> = x.0.iter().zip(&x.1).map(|(a, c)| a + c).collect();
    let d: Vec<f64> = (0..mid.len()).map(|i| mid[i] - if i > 0 { mid[i - 1] } else { 0.0 }).collect();
    let e: Vec<f64> = d.chunks(b).map(|c| c.iter().map(|v| v * v).sum::<f64>() / b as f64).collect();
    let peak = e.iter().cloned().fold(0.0, f64::max);
    let mut out: Vec<f64> = Vec::new();
    for i in 10..e.len() {
        let before = e[i - 10..i].iter().sum::<f64>() / 10.0;
        if e[i] > before * 7.94 && e[i] > peak * 3.16e-5 && out.last().is_none_or(|&o| (i * b) as f64 - o > 0.030 * SR) {
            // The first sample of the block above a fifth of its peak.
            let blk = &x.0[i * b..(i * b + b).min(x.0.len())];
            let m = blk.iter().zip(&x.1[i * b..]).map(|(l, r)| (l + r).abs()).fold(0.0, f64::max);
            let k = blk.iter().zip(&x.1[i * b..]).position(|(l, r)| (l + r).abs() >= 0.2 * m).unwrap_or(0);
            out.push((i * b + k) as f64);
        }
    }
    out
}

// ---------------------------------------------------------------- WSOLA

const WS_N: usize = 1024;
const WS_HS: usize = 512;
const WS_TOL: isize = 256;

fn wsola(src: &Stereo, map: &Map, out_len: usize) -> Stereo {
    let w = hann(WS_N);
    let n_in = src.0.len() as isize;
    let at = |c: &Vec<f64>, i: isize| if i >= 0 && i < n_in { c[i as usize] } else { 0.0 };
    let mid = |i: isize| at(&src.0, i) + at(&src.1, i);
    let mut out = (vec![0.0; out_len + WS_N], vec![0.0; out_len + WS_N]);
    let mut wsum = vec![0.0; out_len + WS_N];
    let mut prev: Option<isize> = None;
    let mut k = 0usize;
    while k * WS_HS < out_len + WS_HS {
        let y = (k * WS_HS) as isize - (WS_N / 2) as isize;
        let nominal = (map.src(y as f64 + (WS_N / 2) as f64) - (WS_N / 2) as f64).round() as isize;
        let start = match prev {
            None => nominal,
            Some(p) => {
                // The continuation of the last frame, over the overlap.
                let target = p + WS_HS as isize;
                let ov = WS_N - WS_HS;
                let score = |d: isize| {
                    let (mut xy, mut yy) = (0.0, 0.0);
                    for i in 0..ov as isize {
                        let (a, b) = (mid(target + i), mid(nominal + d + i));
                        xy += a * b;
                        yy += b * b;
                    }
                    if yy > 0.0 { xy / yy.sqrt() } else { 0.0 }
                };
                let mut best = (f64::NEG_INFINITY, 0isize);
                let mut d = -WS_TOL;
                while d <= WS_TOL {
                    let s = score(d);
                    if s > best.0 {
                        best = (s, d);
                    }
                    d += 4;
                }
                for d in best.1 - 3..=best.1 + 3 {
                    let s = score(d);
                    if s > best.0 {
                        best = (s, d);
                    }
                }
                nominal + best.1
            }
        };
        for (i, &wi) in w.iter().enumerate() {
            let o = y + i as isize + (WS_N / 2) as isize;
            if o < 0 || o as usize >= out.0.len() {
                continue;
            }
            let o = o as usize;
            out.0[o] += wi * at(&src.0, start + i as isize);
            out.1[o] += wi * at(&src.1, start + i as isize);
            wsum[o] += wi;
        }
        prev = Some(start);
        k += 1;
    }
    let norm = |v: &mut Vec<f64>| {
        for (x, s) in v.iter_mut().zip(&wsum) {
            if *s > 1e-9 {
                *x /= s;
            }
        }
        v.truncate(out_len);
    };
    norm(&mut out.0);
    norm(&mut out.1);
    out
}

// ----------------------------------------------------------------- PV

/// The phase vocoder's frame (its synthesis hop is a quarter of it).
const PV_N: usize = 4096;

/// `n`: the frame. `resets`: output frames (centres) where the phases
/// restart from the analysis. `cuts`: where transients' zones start, in
/// the source and in the output: a frame centred before one does not see
/// past it, and a frame centred after one that sees it adds nothing
/// before the zone in the output (both faded over 1 ms), so no hit is
/// spread back before itself.
fn pv(src: &Stereo, map: &Map, out_len: usize, cuts: &[(f64, f64, f64)], n: usize) -> Stereo {
    let hs = n / 4;
    let bins = n / 2 + 1;
    let fft = Fft::new(n);
    let w = hann(n);
    let n_in = src.0.len() as isize;
    let at = |c: &Vec<f64>, i: isize| if i >= 0 && i < n_in { c[i as usize] } else { 0.0 };
    let mut out = (vec![0.0; out_len + n], vec![0.0; out_len + n]);
    let mut wsum = vec![0.0; out_len + n];
    let mut phi_a_prev = vec![0.0; bins];
    // The zone the last frame was in, and its transient bins.
    let mut zone: Option<usize> = None;
    let mut flags = vec![false; bins];
    let mut phi_s = vec![0.0; bins];
    let mut prev_x: Option<isize> = None;
    // The analysis window, cut at the first zone start after its centre
    // (faded over the 1 ms after it: inside the zone, before its hit,
    // so the stretch is whole up to where the zone's crossfade ends).
    let fade = 0.001 * SR;
    let gate_at = |x0: isize, z: f64| -> Vec<f64> { (0..n).map(|i| { let t = (x0 + i as isize) as f64; w[i] * ((z + fade - t) / fade).clamp(0.0, 1.0) }).collect() };
    let gate = |x0: isize| -> Vec<f64> {
        let centre = (x0 + (n / 2) as isize) as f64;
        match cuts.iter().find(|&&(z, _, _)| z > centre && z < (x0 + n as isize) as f64) {
            None => w.clone(),
            Some(&(z, _, _)) => gate_at(x0, z),
        }
    };
    // The frames: every hop, and one more centred where each zone starts
    // (gated there): with a ratio above 1 the hop's frames before a zone
    // cannot reach its start (a frame runs at slope 1 inside, the map at
    // 1 / ratio), and this one does.
    let mut centres: Vec<(f64, Option<usize>)> = (0..).map(|k| (k * hs) as f64).take_while(|&c| c < (out_len + hs) as f64).map(|c| (c, None)).collect();
    for (j, &(_, yz, _)) in cuts.iter().enumerate() {
        if yz >= 0.0 {
            centres.push((yz, Some(j)));
        }
    }
    centres.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut prev_centre = 0.0f64;
    let spec = |c: &Vec<f64>, x0: isize, g: &[f64]| {
        let mut re: Vec<f64> = (0..n).map(|i| g[i] * at(c, x0 + i as isize)).collect();
        let mut im = vec![0.0; n];
        fft.run(&mut re, &mut im, false);
        (re, im)
    };
    for &(centre, pre_cut) in &centres {
        let x0 = (map.src(centre) - (n / 2) as f64).round() as isize;
        let g = match pre_cut {
            Some(j) => gate_at(x0, cuts[j].0),
            None => gate(x0),
        };
        let hsyn = centre - prev_centre;
        let (ml, mi_) = {
            // The extra frame at a zone start measures its phases with the
            // whole window (a cut one would bias them) and only its output
            // is cut.
            let aw: &[f64] = if pre_cut.is_some() { &w } else { &g };
            let mut re: Vec<f64> = (0..n).map(|i| aw[i] * (at(&src.0, x0 + i as isize) + at(&src.1, x0 + i as isize))).collect();
            let mut im = vec![0.0; n];
            fft.run(&mut re, &mut im, false);
            (re, im)
        };
        let mag: Vec<f64> = (0..bins).map(|b| (ml[b] * ml[b] + mi_[b] * mi_[b]).sqrt()).collect();
        let phi_a: Vec<f64> = (0..bins).map(|b| math::atan2(mi_[b], ml[b])).collect();
        // Peaks and their regions (bounded by the lowest bin between).
        let peaks: Vec<usize> = (0..bins).filter(|&b| (b.saturating_sub(2)..(b + 3).min(bins)).all(|c| c == b || mag[c] < mag[b]) && mag[b] > 1e-9).collect();
        let mut peak_of = vec![usize::MAX; bins];
        for (j, &p) in peaks.iter().enumerate() {
            let lo = if j == 0 { 0 } else { (peaks[j - 1]..=p).min_by(|&a, &b| mag[a].total_cmp(&mag[b])).unwrap() };
            let hi = if j + 1 == peaks.len() { bins - 1 } else { (p..=peaks[j + 1]).min_by(|&a, &b| mag[a].total_cmp(&mag[b])).unwrap() };
            for o in peak_of.iter_mut().take(hi + 1).skip(lo) {
                *o = p;
            }
        }
        let reset = prev_x.is_none();
        let mut next = vec![0.0; bins];
        if reset {
            next.copy_from_slice(&phi_a);
        } else {
            let ha = (x0 - prev_x.unwrap()) as f64;
            for &p in &peaks {
                let wb = TAU * p as f64 / n as f64;
                let mut d = phi_a[p] - phi_a_prev[p] - wb * ha;
                d -= TAU * (d / TAU).round();
                let omega = wb + if ha != 0.0 { d / ha } else { 0.0 };
                next[p] = phi_s[p] + omega * hsyn;
            }
            for b in 0..bins {
                let p = peak_of[b];
                if p != usize::MAX && p != b {
                    next[b] = phi_a[b] + (next[p] - phi_a[p]);
                } else if p == usize::MAX {
                    next[b] = phi_a[b];
                }
            }
        }
        // In a transient's zone, the bins whose energy at its first frame
        // is more than 6 dB over a whole window ending where the zone
        // starts (12 dB near a tone of that window) take the source's own
        // phases: the transient as it is, the tones under it carried on.
        let here = if pre_cut.is_some() { zone } else { cuts.iter().position(|&(_, y0, y1)| centre >= y0 && centre < y1) };
        if here != zone {
            if let Some(j) = here {
                // The reference: a whole window ending where the zone
                // starts (it holds the tones, not the hit).
                let z = cuts[j].0 as isize;
                let x_ref = z - n as isize;
                let mut re: Vec<f64> = (0..n).map(|i| w[i] * (at(&src.0, x_ref + i as isize) + at(&src.1, x_ref + i as isize))).collect();
                let mut im = vec![0.0; n];
                fft.run(&mut re, &mut im, false);
                let rm: Vec<f64> = (0..bins).map(|b| (re[b] * re[b] + im[b] * im[b]).sqrt()).collect();
                // Bins near a clear peak of the reference (above -40 dB of
                // its loudest, within 3 bins: the window's main lobe and a
                // margin) are a tone that goes on: they keep their phases.
                let top = rm.iter().cloned().fold(0.0, f64::max);
                let mut tonal = vec![false; bins];
                for b in 0..bins {
                    let peak = rm[b] > 0.01 * top && (b.saturating_sub(2)..(b + 3).min(bins)).all(|c| c == b || rm[c] < rm[b]);
                    if peak {
                        for t in tonal.iter_mut().take((b + 4).min(bins)).skip(b.saturating_sub(3)) {
                            *t = true;
                        }
                    }
                }
                for b in 0..bins {
                    // A tone's bin is the hit's when the hit brings 12 dB
                    // more than the tone had (a new drum's body over the
                    // last one's tail).
                    let rise = if tonal[b] { 4.0 } else { 2.0 };
                    flags[b] = mag[b] > rise * rm[b] && mag[b] > 1e-9;
                }
            }
            zone = here;
        }
        if zone.is_some() {
            for b in 0..bins {
                if flags[b] {
                    next[b] = phi_a[b];
                }
            }
        }
        // Each channel turned by the mid's rotation.
        let y0 = centre as isize - (n / 2) as isize;
        // A frame centred at or after a zone start that sees it: nothing
        // before the zone's start in the output.
        let src_centre = (x0 + (n / 2) as isize) as f64;
        let sg: Vec<f64> = match cuts.iter().rev().find(|&&(z, _, _)| pre_cut.is_none() && z <= src_centre && z >= x0 as f64) {
            None => vec![1.0; n],
            Some(&(_, yz, _)) => (0..n).map(|i| (((y0 + i as isize) as f64 - yz) / (0.001 * SR)).clamp(0.0, 1.0)).collect(),
        };
        for (ch, dst) in [(&src.0, 0usize), (&src.1, 1usize)] {
            let (re, im) = spec(ch, x0, &g);
            let mut yr = vec![0.0; n];
            let mut yi = vec![0.0; n];
            for b in 0..bins {
                let rot = next[b] - phi_a[b];
                let (s, c) = math::sin_cos(rot);
                yr[b] = re[b] * c - im[b] * s;
                yi[b] = re[b] * s + im[b] * c;
                if b > 0 && b < n / 2 {
                    yr[n - b] = yr[b];
                    yi[n - b] = -yi[b];
                }
            }
            fft.run(&mut yr, &mut yi, true);
            let o = if dst == 0 { &mut out.0 } else { &mut out.1 };
            for i in 0..n {
                let t = y0 + i as isize;
                if t >= 0 && (t as usize) < o.len() {
                    o[t as usize] += sg[i] * w[i] * yr[i] / n as f64;
                    if dst == 0 {
                        wsum[t as usize] += sg[i] * w[i] * g[i];
                    }
                }
            }
        }
        // The extra frame borrows the chain's phases and leaves them.
        if pre_cut.is_none() {
            phi_a_prev = phi_a;
            phi_s = next;
            prev_x = Some(x0);
            prev_centre = centre;
        }
    }
    let norm = |v: &mut Vec<f64>| {
        for (x, s) in v.iter_mut().zip(&wsum) {
            if *s > 1e-6 {
                *x /= s;
            }
        }
        v.truncate(out_len);
    };
    norm(&mut out.0);
    norm(&mut out.1);
    out
}

// ------------------------------------------------------------ algorithms

#[derive(Clone, Copy, PartialEq)]
enum Alg {
    Wsola,
    Pv,
    WsolaT,
    PvT,
}

const ALGS: [Alg; 4] = [Alg::Wsola, Alg::Pv, Alg::WsolaT, Alg::PvT];

fn name(a: Alg) -> &'static str {
    match a {
        Alg::Wsola => "WSOLA",
        Alg::Pv => "PV",
        Alg::WsolaT => "WSOLA+T",
        Alg::PvT => "PV+T",
    }
}

/// The transient zones of `on` (source onsets): from 5 ms before to 40
/// ms after, in the output the source itself (unstretched, the onset at
/// t * ratio), crossfaded over 2 ms at each edge with what the stretch
/// made there.
fn keep_transients(out: &mut Stereo, src: &Stereo, on: &[f64], ratio: f64) {
    let (pre, post, fade) = (0.005 * SR, 0.040 * SR, 0.002 * SR);
    let map = Map::transients(ratio, on);
    for &o in on {
        let (y0, y1) = (o * ratio - pre, o * ratio + post);
        // A zone the map skipped (onsets too close) is not kept.
        if (map.src(o * ratio) - o).abs() > 0.5 {
            continue;
        }
        let d = o - o * ratio;
        let a = (y0 - fade).max(0.0) as usize;
        let b = ((y1 + fade) as usize).min(out.0.len());
        for y in a..b {
            let yf = y as f64;
            let g = if yf < y0 { (yf - (y0 - fade)) / fade } else if yf > y1 { 1.0 - (yf - y1) / fade } else { 1.0 };
            let x = (yf + d).round() as isize;
            let sv = |c: &Vec<f64>| if x >= 0 && (x as usize) < c.len() { c[x as usize] } else { 0.0 };
            out.0[y] += g * (sv(&src.0) - out.0[y]);
            out.1[y] += g * (sv(&src.1) - out.1[y]);
        }
    }
}

fn stretch(a: Alg, src: &Stereo, ratio: f64) -> Stereo {
    stretch_n(a, src, ratio, PV_N)
}

fn stretch_n(a: Alg, src: &Stereo, ratio: f64, n: usize) -> Stereo {
    let out_len = (src.0.len() as f64 * ratio).round() as usize;
    match a {
        Alg::Wsola => wsola(src, &Map::uniform(ratio), out_len),
        Alg::Pv => pv(src, &Map::uniform(ratio), out_len, &[], n),
        Alg::WsolaT => {
            let on = onsets(src);
            let mut y = wsola(src, &Map::transients(ratio, &on), out_len);
            keep_transients(&mut y, src, &on, ratio);
            y
        }
        Alg::PvT => {
            let on = onsets(src);
            let map = Map::transients(ratio, &on);
            // Zones the map kept (onsets too close for the ratio are not).
            let cuts: Vec<(f64, f64, f64)> = on.iter().filter(|&&o| (map.src(o * ratio) - o).abs() < 0.5).map(|o| (o - 0.001 * SR, o * ratio - 0.001 * SR, o * ratio + 0.040 * SR)).collect();
            pv(src, &map, out_len, &cuts, n)
        }
    }
}

/// Resample `x` reading `step` source frames per output frame (32-tap
/// Kaiser windowed sinc, cut-off 0.45 of the slower rate).
fn resample(x: &[f64], step: f64, out_len: usize) -> Vec<f64> {
    let fc = 0.45 / step.max(1.0);
    let half = (16.0 * step.max(1.0)).ceil() as isize;
    let i0 = |v: f64| {
        let (mut s, mut t, mut k) = (1.0f64, 1.0f64, 1.0f64);
        let q = v * v / 4.0;
        while t > 1e-17 * s {
            t *= q / (k * k);
            s += t;
            k += 1.0;
        }
        s
    };
    let ib = i0(8.0);
    (0..out_len)
        .map(|j| {
            let pos = j as f64 * step;
            let c = pos.floor() as isize;
            let mut acc = 0.0;
            for t in c - half + 1..=c + half {
                if t < 0 || t as usize >= x.len() {
                    continue;
                }
                let d = t as f64 - pos;
                let r = d / half as f64;
                if r.abs() >= 1.0 {
                    continue;
                }
                let sinc = if d.abs() < 1e-12 { 1.0 } else { math::sin(std::f64::consts::PI * 2.0 * fc * d) / (std::f64::consts::PI * 2.0 * fc * d) };
                acc += x[t as usize] * 2.0 * fc * sinc * i0(8.0 * (1.0 - r * r).sqrt()) / ib;
            }
            acc
        })
        .collect()
}

/// Ratio `ratio` and `semis` of shift.
fn process(a: Alg, src: &Stereo, ratio: f64, semis: f64) -> Stereo {
    if semis == 0.0 {
        return stretch(a, src, ratio);
    }
    let s = math::exp2(semis / 12.0);
    let st = stretch(a, src, ratio * s);
    let out_len = (src.0.len() as f64 * ratio).round() as usize;
    (resample(&st.0, s, out_len), resample(&st.1, s, out_len))
}

// ----------------------------------------------------------------- signals

fn tone(freqs: &[(f64, f64)], secs: f64) -> Vec<f64> {
    (0..(secs * SR) as usize).map(|i| freqs.iter().map(|&(f, a)| a * math::sin(TAU * f * i as f64 / SR)).sum()).collect()
}

/// A held harmonic tone at 220 Hz (6 partials, 1/k).
fn held() -> Stereo {
    let p: Vec<(f64, f64)> = (1..=6).map(|k| (220.0 * k as f64, 0.25 / k as f64)).collect();
    let x = tone(&p, 4.0);
    (x.clone(), x)
}

const CHORD: [f64; 3] = [261.63, 329.63, 392.0];

fn chord() -> Stereo {
    let x = tone(&CHORD.map(|f| (f, 0.2)), 4.0);
    (x.clone(), x)
}

/// Clicks every 0.25 s from 0.5 s (one sample, 0.8).
fn clicks() -> (Stereo, Vec<f64>) {
    let n = (4.0 * SR) as usize;
    let mut x = vec![0.0; n];
    let mut at = Vec::new();
    let mut t = (0.5 * SR) as usize;
    while t < n - 12_000 {
        x[t] = 0.8;
        at.push(t as f64);
        t += 12_000;
    }
    ((x.clone(), x), at)
}

/// A drum hit: noise with a 0.5 ms rise and a 60 ms decay, a 55 Hz thump
/// with 120 ms; at irregular times (a loop's hits) from 0.5 s.
fn drums() -> (Stereo, Vec<f64>) {
    let n = (4.0 * SR) as usize;
    let mut x = vec![0.0; n];
    let times = [0.5, 0.75, 1.125, 1.5, 1.6875, 2.0, 2.5, 2.8125, 3.25];
    let mut seed = 12345u32;
    for &t in &times {
        let s = (t * SR) as usize;
        for i in 0..(0.4 * SR) as usize {
            if s + i >= n {
                break;
            }
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed >> 8) as f64 / 8_388_608.0 - 1.0;
            let tt = i as f64 / SR;
            let rise = (tt / 0.0005).min(1.0);
            x[s + i] += rise * (0.5 * noise * math::exp(-tt / 0.060) + 0.5 * math::sin(TAU * 55.0 * tt) * math::exp(-tt / 0.120));
        }
    }
    ((x.clone(), x), times.iter().map(|t| t * SR).collect())
}

/// A wide source: 330 Hz with the right channel 60 degrees ahead, 1250 Hz
/// with it 45 degrees behind.
fn wide() -> Stereo {
    let n = (4.0 * SR) as usize;
    let l = (0..n).map(|i| { let t = i as f64 / SR; 0.3 * math::sin(TAU * 330.0 * t) + 0.2 * math::sin(TAU * 1250.0 * t) }).collect();
    let r = (0..n).map(|i| { let t = i as f64 / SR; 0.3 * math::sin(TAU * 330.0 * t + 60f64.to_radians()) + 0.2 * math::sin(TAU * 1250.0 * t - 45f64.to_radians()) }).collect();
    (l, r)
}

// ----------------------------------------------------------------- measures

/// Amplitude and phase of `x` at `f` Hz (least squares fit of a sine).
fn fit(x: &[f64], f: f64) -> (f64, f64) {
    let w = TAU * f / SR;
    let (mut cc, mut ss, mut cs, mut xc, mut xs) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (n, &v) in x.iter().enumerate() {
        let (s, c) = math::sin_cos(w * n as f64);
        cc += c * c;
        ss += s * s;
        cs += c * s;
        xc += c * v;
        xs += s * v;
    }
    let d = cc * ss - cs * cs;
    let (a, b) = ((xc * ss - xs * cs) / d, (xs * cc - xc * cs) / d);
    ((a * a + b * b).sqrt(), math::atan2(a, b))
}

/// The frequency near `f` (within 2 %) where a Hann-windowed DFT of `x`
/// peaks (golden-section search).
fn freq(x: &[f64], f: f64) -> f64 {
    let w = hann(x.len());
    let power = |g: f64| {
        let om = TAU * g / SR;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &v) in x.iter().enumerate() {
            let (s, c) = math::sin_cos(om * n as f64);
            re += v * w[n] * c;
            im += v * w[n] * s;
        }
        re * re + im * im
    };
    let (mut a, mut b) = (f * 0.98, f * 1.02);
    // Coarse scan first (other partials), then golden section.
    let mut best = (0.0, f);
    for k in 0..=80 {
        let g = a + (b - a) * k as f64 / 80.0;
        let p = power(g);
        if p > best.0 {
            best = (p, g);
        }
    }
    let span = (b - a) / 80.0;
    (a, b) = (best.1 - span, best.1 + span);
    let gr = 0.618_033_988_75;
    for _ in 0..40 {
        let (c, d) = (b - gr * (b - a), a + gr * (b - a));
        if power(c) > power(d) {
            b = d;
        } else {
            a = c;
        }
    }
    (a + b) / 2.0
}

fn cents(f: f64, want: f64) -> f64 {
    1200.0 * math::log2(f / want)
}

/// The steady middle second of a signal of output length.
fn middle(x: &[f64]) -> &[f64] {
    let c = x.len() / 2;
    &x[c - (0.5 * SR) as usize..c + (0.5 * SR) as usize]
}

/// Modulation a steady chord's notes carry: each note brought to 0 Hz
/// (heterodyne) and low-passed (two 60 ms moving averages: nulls every
/// 16.7 Hz, below -45 dB at the 46 Hz to the nearest note of the chord
/// shifted down 5 semitones), its magnitude every 1 ms over the steady
/// middle 1 s; the depth (standard deviation / mean). Roughness and
/// phasiness show up as modulation of the partials at the frame rate and
/// its beats; the source's own depth is the floor of the measure. dB.
fn modulation(x: &[f64], f: f64) -> f64 {
    let w = TAU * f / SR;
    let c = x.len() / 2;
    let span = (0.5 * SR) as usize;
    let (a, b) = (c - span, c + span);
    let avg = (0.060 * SR) as usize;
    let mut re: Vec<f64> = (a - 2 * avg..b).map(|n| x[n] * math::cos(w * n as f64)).collect();
    let mut im: Vec<f64> = (a - 2 * avg..b).map(|n| x[n] * math::sin(w * n as f64)).collect();
    for _ in 0..2 {
        for v in [&mut re, &mut im] {
            let mut acc = 0.0;
            let src = v.clone();
            for i in 0..v.len() {
                acc += src[i];
                if i >= avg {
                    acc -= src[i - avg];
                }
                v[i] = acc / avg as f64;
            }
        }
    }
    let mags: Vec<f64> = (2 * avg..re.len()).step_by(48).map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt()).collect();
    let mean = mags.iter().sum::<f64>() / mags.len() as f64;
    let sd = (mags.iter().map(|m| (m - mean) * (m - mean)).sum::<f64>() / mags.len() as f64).sqrt();
    20.0 * math::log10(sd / mean)
}

/// Around an event expected at `at` (output frames): the time holding the
/// middle 90 % of the energy from 50 ms before to 100 ms after (ms), and
/// the energy from 50 ms to 1 ms before it against the whole (dB:
/// pre-echo).
fn smear(x: &[f64], at: f64) -> (f64, f64) {
    let (a, b) = ((at - 0.050 * SR).max(0.0) as usize, ((at + 0.100 * SR) as usize).min(x.len()));
    let e: Vec<f64> = x[a..b].iter().map(|v| v * v).collect();
    let total: f64 = e.iter().sum();
    let mut acc = 0.0;
    let (mut lo, mut hi) = (0, e.len());
    for (i, v) in e.iter().enumerate() {
        acc += v;
        if acc < 0.05 * total {
            lo = i;
        }
        if acc >= 0.95 * total {
            hi = i;
            break;
        }
    }
    let pre_end = ((at - 0.001 * SR) as usize).saturating_sub(a);
    let pre: f64 = e[..pre_end.min(e.len())].iter().sum();
    ((hi - lo) as f64 / SR * 1000.0, 10.0 * math::log10((pre / total).max(1e-15)))
}

/// Attack time of a hit expected at `at`: the envelope (peak |x| in 0.5
/// ms blocks) from 10 % to 90 % of its maximum within 30 ms before to
/// 50 ms after (ms).
fn attack(x: &[f64], at: f64) -> f64 {
    let blk = (0.0005 * SR) as usize;
    let (a, b) = ((at - 0.030 * SR).max(0.0) as usize, ((at + 0.050 * SR) as usize).min(x.len()));
    let env: Vec<f64> = x[a..b].chunks(blk).map(|c| c.iter().map(|v| v.abs()).fold(0.0, f64::max)).collect();
    let m = env.iter().cloned().fold(0.0, f64::max);
    let t10 = env.iter().position(|&v| v >= 0.1 * m).unwrap_or(0);
    let t90 = env.iter().position(|&v| v >= 0.9 * m).unwrap_or(0);
    (t90.saturating_sub(t10)) as f64 * 0.5
}

// ----------------------------------------------------------------- main

/// Ratio 1 without the bypass, by region: far from every transient
/// zone (more than 50 ms: the tonal part), before a zone (the 50 ms up
/// to its crossfade), the zone with its crossfades, after it (50 ms).
/// What remains around hits away from ratio 1: a held 330 Hz tone (0.3)
/// with a short noise hit (0.8, 8 ms decay) every 0.25 s on top. Where a
/// zone starts (5 ms before its hit) the stretched tone, its phase the
/// vocoder's, crossfades into the source's own tone; unless their phases
/// agree, the tone's level dips there. Where the zone ends the vocoder
/// restarts from the source's phases. The tone's envelope (heterodyned
/// at 330 Hz, two moving averages of 3.03 ms: nulls on 330 Hz and its
/// image at 660 Hz) against its median away from hits: the deepest dip
/// before each hit (from 25 ms to 3 ms before), within 3 ms of it (where
/// the hit's own energy in the tone's bins, turned with the tone, adds to
/// it), and after its zone (from 38 ms to 70 ms after it), dB.
fn tone_through_zones() {
    let n = (4.0 * SR) as usize;
    let mut x: Vec<f64> = (0..n).map(|i| 0.3 * math::sin(TAU * 330.0 * i as f64 / SR)).collect();
    let mut seed = 777u32;
    let mut t = (0.5 * SR) as usize;
    while t < n - 12_000 {
        for i in 0..(0.06 * SR) as usize {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed >> 8) as f64 / 8_388_608.0 - 1.0;
            x[t + i] += 0.8 * noise * math::exp(-(i as f64) / (0.008 * SR));
        }
        t += 12_000;
    }
    let src = (x.clone(), x);
    let on = onsets(&src);
    println!("\na held 330 Hz tone under noise hits 8.5 dB above it ({} onsets found), PV+T: the tone's deepest dip from 25 to 3 ms before each hit / within 3 ms of it (under the hit) / from 38 to 70 ms after it (dB)", on.len());
    for r in [1.0, 1.01, 1.02, 1.1, 1.25, 1.5, 2.0, 0.9, 0.75, 0.5] {
        let y = stretch(Alg::PvT, &src, r);
        let w = TAU * 330.0 / SR;
        let avg = 145usize;
        let mut re: Vec<f64> = y.0.iter().enumerate().map(|(i, v)| v * math::cos(w * i as f64)).collect();
        let mut im: Vec<f64> = y.0.iter().enumerate().map(|(i, v)| v * math::sin(w * i as f64)).collect();
        for _ in 0..2 {
            for v in [&mut re, &mut im] {
                let src = v.clone();
                let mut acc = 0.0;
                for i in 0..v.len() {
                    acc += src[i];
                    if i >= avg {
                        acc -= src[i - avg];
                    }
                    v[i] = acc / avg as f64;
                }
            }
        }
        let env: Vec<f64> = re.iter().zip(&im).map(|(a, b)| 2.0 * (a * a + b * b).sqrt()).collect();
        let mut steady: Vec<f64> = (0..env.len()).step_by(48).filter(|&i| i > (0.3 * SR) as usize && i < env.len() - (0.3 * SR) as usize && on.iter().all(|&o| (i as f64 - o * r).abs() > 0.06 * SR)).map(|i| env[i]).collect();
        steady.sort_by(f64::total_cmp);
        let med = steady[steady.len() / 2];
        let dip = |a: f64, b: f64| {
            on.iter().map(|&o| {
                let (lo, hi) = (((o * r + a * SR).max(0.0)) as usize, ((o * r + b * SR) as usize).min(env.len()));
                env[lo..hi].iter().cloned().fold(f64::MAX, f64::min)
            }).fold(f64::MAX, f64::min)
        };
        println!("  x{r}: {:.2} / {:.2} / {:.2}", 20.0 * math::log10(dip(-0.025, -0.003) / med), 20.0 * math::log10(dip(-0.003, 0.003) / med), 20.0 * math::log10(dip(0.038, 0.070) / med));
    }
}

fn null_by_region() {
    let (dr, c) = (drums().0, chord());
    {
        let mix: Stereo = (dr.0.iter().zip(&c.0).map(|(a, b)| a + b).collect(), dr.1.iter().zip(&c.1).map(|(a, b)| a + b).collect());
        let on = onsets(&mix);
        let y = stretch(Alg::PvT, &mix, 1.0);
        let (pre, post, xf, near) = (0.005 * SR, 0.040 * SR, 0.002 * SR, 0.050 * SR);
        let mut worst = [0.0f64; 4];
        for t in 0..mix.0.len() {
            let tf = t as f64;
            let d = (y.0[t] - mix.0[t]).abs().max((y.1[t] - mix.1[t]).abs());
            let mut class = 0;
            for &o in &on {
                let (z0, z1) = (o - pre - xf, o + post + xf);
                if tf >= z0 && tf <= z1 {
                    class = 2;
                    break;
                } else if tf < z0 && tf >= z0 - near {
                    class = class.max(1);
                } else if tf > z1 && tf <= z1 + near {
                    class = class.max(3);
                }
            }
            worst[class] = worst[class].max(d);
        }
        let db = |v: f64| 20.0 * math::log10(v.max(1e-300));
        println!("\nratio 1, PV+T without the bypass, drums + chord: largest difference by region (dB FS): far from hits {:.1}, the 50 ms before a zone {:.1}, the zone with its crossfades {:.1}, the 50 ms after {:.1}", db(worst[0]), db(worst[1]), db(worst[2]), db(worst[3]));
    }
}


fn main() {
    if std::env::var("TS_NULL").is_ok() {
        null_by_region();
        tone_through_zones();
        return;
    }

    let ratios = [0.5, 0.75, 0.9, 1.01, 1.02, 1.1, 1.25, 1.5, 2.0];
    let shifts = [-12.0, -5.0, 5.0, 12.0];
    let cases: Vec<(f64, f64)> = ratios.iter().map(|&r| (r, 0.0)).chain(shifts.iter().map(|&s| (1.0, s))).collect();
    let head = |what: &str| {
        println!("\n{what}");
        print!("{:>14}", "ratio / shift");
        for a in ALGS {
            print!(" {:>10}", name(a));
        }
        println!();
    };
    let row = |r: f64, s: f64, vals: &[String]| {
        let label = if s == 0.0 { format!("x{r}") } else { format!("{s:+} st") };
        print!("{label:>14}");
        for v in vals {
            print!(" {v:>10}");
        }
        println!();
    };

    // Pitch: a held tone and a chord.
    let (h, c) = (held(), chord());
    head("pitch error, held 220 Hz tone (cents, worst of partials 1-3)");
    for &(r, s) in &cases {
        let p = math::exp2(s / 12.0);
        let v: Vec<String> = ALGS.iter().map(|&a| { let y = process(a, &h, r, s); let m = middle(&y.0); (1..=3).map(|k| cents(freq(m, 220.0 * k as f64 * p), 220.0 * k as f64 * p).abs()).fold(0.0, f64::max) }).map(|e| format!("{e:.3}")).collect();
        row(r, s, &v);
    }
    head("pitch error, chord C-E-G (cents, worst note)");
    for &(r, s) in &cases {
        let p = math::exp2(s / 12.0);
        let v: Vec<String> = ALGS.iter().map(|&a| { let y = process(a, &c, r, s); let m = middle(&y.0); CHORD.iter().map(|f| cents(freq(m, f * p), f * p).abs()).fold(0.0, f64::max) }).map(|e| format!("{e:.3}")).collect();
        row(r, s, &v);
    }

    // Modulation not in the source, steady chord.
    head("modulation of the chord's notes (dB, worst note; last column: the measure's floor, the ideal chord at that pitch)");
    for &(r, s) in &cases {
        let p = math::exp2(s / 12.0);
        let mut v: Vec<String> = ALGS.iter().map(|&a| { let y = process(a, &c, r, s); CHORD.iter().map(|&f| modulation(&y.0, f * p)).fold(f64::NEG_INFINITY, f64::max) }).map(|e| format!("{e:.1}")).collect();
        let ideal = tone(&CHORD.map(|f| (f * p, 0.2)), 4.0 * r);
        v.push(format!("{:.1}", CHORD.iter().map(|&f| modulation(&ideal, f * p)).fold(f64::NEG_INFINITY, f64::max)));
        row(r, s, &v);
    }

    println!("(PV with a 2048 frame instead of 4096: the same measure)");
    for &(r, s) in &cases {
        let p = math::exp2(s / 12.0);
        let y = if s == 0.0 { stretch_n(Alg::Pv, &c, r, 2048) } else { let q = stretch_n(Alg::Pv, &c, r * p, 2048); let n = (c.0.len() as f64 * r).round() as usize; (resample(&q.0, p, n), resample(&q.1, p, n)) };
        let v = CHORD.iter().map(|&f| modulation(&y.0, f * p)).fold(f64::NEG_INFINITY, f64::max);
        row(r, s, &[format!("{v:.1}")]);
    }

    // Transients.
    let (ck, ck_at) = clicks();
    let (dr, dr_at) = drums();
    let src_smear = ck_at.iter().map(|&t| smear(&ck.0, t).0).fold(0.0, f64::max);
    head(&format!("click smear (ms holding 90 % of the energy, worst; the source: {src_smear:.2})"));
    let mut pre_clicks = Vec::new();
    for &(r, s) in &cases {
        let mut pre = Vec::new();
        let v: Vec<String> = ALGS.iter().map(|&a| {
            let y = process(a, &ck, r, s);
            let m = ck_at.iter().map(|&t| smear(&y.0, t * r)).fold((0.0f64, -200.0f64), |acc, x| (acc.0.max(x.0), acc.1.max(x.1)));
            pre.push(format!("{:.1}", m.1));
            format!("{:.2}", m.0)
        }).collect();
        row(r, s, &v);
        pre_clicks.push((r, s, pre));
    }
    head("click pre-echo (dB of the event's energy from 50 ms to 1 ms before it, worst)");
    for (r, s, v) in &pre_clicks {
        row(*r, *s, v);
    }
    let src_attack = dr_at.iter().map(|&t| attack(&dr.0, t)).fold(0.0, f64::max);
    let src_pre = dr_at.iter().map(|&t| smear(&dr.0, t).1).fold(-200.0, f64::max);
    head(&format!("drum hit attack, 10-90 % (ms, worst; the source: {src_attack:.1})"));
    let mut pre_drums = Vec::new();
    let mut timing = Vec::new();
    let src_on = onsets(&dr);
    for &(r, s) in &cases {
        let mut pre = Vec::new();
        let mut tim = Vec::new();
        let v: Vec<String> = ALGS.iter().map(|&a| {
            let y = process(a, &dr, r, s);
            let att = dr_at.iter().map(|&t| attack(&y.0, t * r)).fold(0.0, f64::max);
            pre.push(format!("{:.1}", dr_at.iter().map(|&t| smear(&y.0, t * r).1).fold(-200.0, f64::max)));
            // Timing: onsets found the same way in source and output.
            let out_on = onsets(&y);
            let err = src_on.iter().map(|&o| out_on.iter().map(|&q| (q - o * r).abs()).fold(f64::MAX, f64::min)).fold(0.0, f64::max);
            tim.push(format!("{err:.0}"));
            format!("{att:.1}")
        }).collect();
        row(r, s, &v);
        pre_drums.push((r, s, pre));
        timing.push((r, s, tim));
    }
    head(&format!("drum pre-echo (dB, worst; the source: {src_pre:.1})"));
    for (r, s, v) in &pre_drums {
        row(*r, *s, v);
    }
    head("onset timing: |onset found - source onset x ratio| (frames, worst of 9 hits)");
    for (r, s, v) in &timing {
        row(*r, *s, v);
    }

    // Stereo.
    let wd = wide();
    head("inter-channel phase error (degrees, worst of 330 Hz and 1250 Hz)");
    for &(r, s) in &cases {
        let p = math::exp2(s / 12.0);
        let v: Vec<String> = ALGS.iter().map(|&a| {
            let y = process(a, &wd, r, s);
            let (ml, mr) = (middle(&y.0), middle(&y.1));
            [(330.0, 60.0), (1250.0, -45.0)].iter().map(|&(f, want)| {
                let d = (fit(mr, f * p).1 - fit(ml, f * p).1).to_degrees();
                let e = (d - want + 540.0).rem_euclid(360.0) - 180.0;
                e.abs()
            }).fold(0.0, f64::max)
        }).map(|e| format!("{e:.2}")).collect();
        row(r, s, &v);
    }

    null_by_region();
    tone_through_zones();

    // Identity at ratio 1, no shift.
    println!("\nratio 1, no shift: largest difference from the source (dB of full scale; a bypass is bit-identical)");
    let mix: Stereo = (dr.0.iter().zip(&c.0).map(|(a, b)| a + b).collect(), dr.1.iter().zip(&c.1).map(|(a, b)| a + b).collect());
    for a in ALGS {
        let y = stretch(a, &mix, 1.0);
        let d = y.0.iter().zip(&mix.0).map(|(p, q)| (p - q).abs()).fold(0.0, f64::max);
        println!("{:>14} {:.1}", name(a), 20.0 * math::log10(d.max(1e-300)));
    }

    // Cost: drums + chord, 4 s stereo, ns per output frame (both channels).
    println!("\ncost (ns per output frame, stereo; this harness: f64, allocating per frame, unoptimised: an upper bound)");
    print!("{:>14}", "ratio");
    for a in ALGS {
        print!(" {:>10}", name(a));
    }
    println!();
    for &r in &[0.5, 1.0, 1.5, 2.0] {
        print!("{:>14}", format!("x{r}"));
        for a in ALGS {
            let t0 = Instant::now();
            let y = stretch(a, &mix, r);
            let dt = t0.elapsed().as_secs_f64();
            print!(" {:>10.0}", dt * 1e9 / y.0.len() as f64);
        }
        println!();
    }
    // The parts a streaming phase vocoder (frame 4096, hop 1024) would
    // spend per hop, timed here (f64): one complex FFT of 4096 (the two
    // channels packed as real and imaginary: their spectra and the mid's
    // follow), 2049 atan2 (the mid's phases), 2049 sin_cos (one rotation
    // per bin serves both channels), one inverse complex FFT.
    let fft = Fft::new(4096);
    let (mut re, mut im): (Vec<f64>, Vec<f64>) = ((0..4096).map(|i| math::sin(i as f64 * 0.01)).collect(), vec![0.0; 4096]);
    let reps = 2_000;
    let t0 = Instant::now();
    for _ in 0..reps {
        fft.run(&mut re, &mut im, false);
        std::hint::black_box(&re);
    }
    let fft_ns = t0.elapsed().as_secs_f64() * 1e9 / reps as f64;
    let t0 = Instant::now();
    let mut acc = 0.0;
    for i in 0..2049 * reps / 10 {
        acc += math::atan2(i as f64 * 1e-3 - 0.7, 0.3);
    }
    let atan_ns = t0.elapsed().as_secs_f64() * 1e9 / (2049 * reps / 10) as f64;
    let t0 = Instant::now();
    for i in 0..2049 * reps / 10 {
        let (a, b) = math::sin_cos(i as f64 * 1e-3);
        acc += a + b;
    }
    let sc_ns = t0.elapsed().as_secs_f64() * 1e9 / (2049 * reps / 10) as f64;
    std::hint::black_box(acc);
    let per_hop = 2.0 * fft_ns + 2049.0 * (atan_ns + sc_ns);
    println!("\nparts (f64, this machine): complex FFT 4096 {fft_ns:.0} ns, atan2 {atan_ns:.1} ns, sin_cos {sc_ns:.1} ns");
    println!("a streaming PV (4096 / hop 1024), stereo, per output frame: ~{:.0} ns at ratio 1 (2 FFTs + 2049 atan2 + 2049 sin_cos per hop of 1024; more analysis per output frame below ratio 1 is not needed: one analysis per synthesis hop)", per_hop / 1024.0);
    let t0 = Instant::now();
    let y = resample(&mix.0, 1.5, mix.0.len());
    println!("resampling for a shift (per output frame, one channel, harness): {:.0} ns", t0.elapsed().as_secs_f64() * 1e9 / y.len() as f64);
}
