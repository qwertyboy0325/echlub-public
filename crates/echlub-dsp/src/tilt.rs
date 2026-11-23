//! Noise colour: a spectral tilt of a constant slope in dB per octave,
//! pivoting at 1 kHz (its gain there is 1).
//!
//! `color` -1..1 is -3..+3 dB per octave: -1 is pink noise's slope, 0 is
//! flat (white; [`Tilt::new`] gives a filter that is off and callers skip
//! it, so a colour of 0 changes no sample), +1 is brighter than white by the
//! same slope (blue-ish).
//!
//! How: [`SECTIONS`] first-order shelves, one every two octaves from 15 Hz
//! (15, 60, 240, 960, 3840, 15360 Hz). A cutting section has its pole at
//! that frequency and its zero `|color|` octaves higher, so each falls 6 dB
//! per octave over a fraction `|color| / 2` of its two octaves: on average
//! `3 |color|` dB per octave. A boosting section swaps the pole and the
//! zero. Every frequency is held at or below 20 kHz (and 0.45 of the
//! rate), so the slope ends there and is flat above at any sample rate (a
//! high rate does not tilt on into the ultrasonic band; what lies above 20
//! kHz keeps the gain at 20 kHz). Bilinear transform with prewarping. Measured against the ideal slope in the
//! tests: within 0.6 dB from 40 Hz to 10 kHz at 44.1, 48 and 96 kHz.
//!
//! The coefficients cost a dozen `tan`s (through [`crate::math`], the same
//! bits everywhere): compute them per hit or per parameter change, not per
//! sample. Ticking is allocation-free and branch-free.

use crate::math;

/// First-order sections.
pub const SECTIONS: usize = 6;
/// The tilt's pivot (gain 1 there), Hz.
pub const PIVOT_HZ: f64 = 1000.0;
/// dB per octave at `color` 1.
pub const DB_PER_OCT: f32 = 3.0;
const LOWEST_HZ: f64 = 15.0;
/// The slope ends here (flat above, at any sample rate).
const TOP_HZ: f64 = 20_000.0;

/// A tilt's coefficients (`y = b0 x + b1 x[-1] - a1 y[-1]` per section).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tilt {
    b0: [f32; SECTIONS],
    b1: [f32; SECTIONS],
    a1: [f32; SECTIONS],
    on: bool,
}

impl Tilt {
    /// No tilt (white stays white).
    pub const FLAT: Tilt = Tilt { b0: [1.0; SECTIONS], b1: [0.0; SECTIONS], a1: [0.0; SECTIONS], on: false };

    /// The tilt for `color` (-1..1, clamped; NaN is 0) at `sample_rate`.
    pub fn new(color: f32, sample_rate: f32) -> Tilt {
        let c = if color.is_nan() { 0.0 } else { color.clamp(-1.0, 1.0) } as f64;
        if c == 0.0 || sample_rate <= 0.0 {
            return Tilt::FLAT;
        }
        let sr = sample_rate as f64;
        let warp = |hz: f64| math::tan(core::f64::consts::PI * hz.min(TOP_HZ).min(0.45 * sr) / sr);
        let ratio = math::exp2(c.abs());
        let w0 = warp(PIVOT_HZ);
        let mut t = Tilt { on: true, ..Tilt::FLAT };
        let mut gain = 1.0f64;
        let mut f = LOWEST_HZ;
        for k in 0..SECTIONS {
            let (lo, hi) = (warp(f), warp(f * ratio));
            // Cut: pole low, zero high; boost: the other way round.
            let (wp, wz) = if c < 0.0 { (lo, hi) } else { (hi, lo) };
            // H(s) = (s + wz) / (s + wp), bilinear: s = (1 - z^-1) / (1 + z^-1).
            let d = 1.0 + wp;
            t.b0[k] = ((1.0 + wz) / d) as f32;
            t.b1[k] = ((wz - 1.0) / d) as f32;
            t.a1[k] = ((wp - 1.0) / d) as f32;
            gain *= ((w0 * w0 + wz * wz) / (w0 * w0 + wp * wp)).sqrt();
            f *= 4.0;
        }
        let g = (1.0 / gain) as f32;
        t.b0[0] *= g;
        t.b1[0] *= g;
        t
    }

    /// Off: a colour of 0 (callers skip it, so the signal is untouched).
    pub fn is_flat(&self) -> bool {
        !self.on
    }
}

impl Default for Tilt {
    fn default() -> Self {
        Tilt::FLAT
    }
}

/// One signal's state through a [`Tilt`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TiltState {
    x1: [f32; SECTIONS],
    y1: [f32; SECTIONS],
}

impl TiltState {
    #[inline]
    pub fn tick(&mut self, t: &Tilt, x: f32) -> f32 {
        let mut v = x;
        for k in 0..SECTIONS {
            let y = t.b0[k] * v + t.b1[k] * self.x1[k] - t.a1[k] * self.y1[k];
            self.x1[k] = v;
            self.y1[k] = y;
            v = y;
        }
        v
    }

    /// Zero states too small to matter (no subnormal arithmetic).
    pub fn flush(&mut self) {
        for s in self.x1.iter_mut().chain(self.y1.iter_mut()) {
            if s.abs() < 1e-20 {
                *s = 0.0;
            }
        }
    }

    pub fn reset(&mut self) {
        *self = TiltState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gain of the cascade at `hz` (its frequency response, exactly).
    fn response(t: &Tilt, hz: f64, sr: f64) -> f64 {
        let w = core::f64::consts::TAU * hz / sr;
        let (s, c) = math::sin_cos(w);
        let mut g = 1.0;
        for k in 0..SECTIONS {
            // (b0 + b1 e^-jw) / (1 + a1 e^-jw)
            let (nr, ni) = (t.b0[k] as f64 + t.b1[k] as f64 * c, -(t.b1[k] as f64) * s);
            let (dr, di) = (1.0 + t.a1[k] as f64 * c, -(t.a1[k] as f64) * s);
            g *= ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt();
        }
        g
    }

    #[test]
    fn zero_is_off_and_the_slope_is_three_db_per_octave_per_unit() {
        assert!(Tilt::new(0.0, 48_000.0).is_flat());
        assert!(Tilt::new(f32::NAN, 48_000.0).is_flat());
        for sr in [44_100.0f64, 48_000.0, 96_000.0] {
            for c in [-1.0f32, -0.5, 0.5, 1.0] {
                let t = Tilt::new(c, sr as f32);
                assert!(!t.is_flat());
                let pivot = 20.0 * math::log10(response(&t, PIVOT_HZ, sr));
                assert!(pivot.abs() < 0.01, "{c} at {sr}: {pivot} dB at the pivot");
                let mut worst = 0.0f64;
                let mut hz = 40.0;
                while hz <= 10_000.0 {
                    let want = (c * DB_PER_OCT) as f64 * math::log2(hz / PIVOT_HZ);
                    let got = 20.0 * math::log10(response(&t, hz, sr));
                    worst = worst.max((got - want).abs());
                    hz *= 1.1;
                }
                assert!(worst < 0.6, "{c} at {sr}: {worst} dB off the slope");
            }
        }
    }

    #[test]
    fn ticking_follows_the_response() {
        // A sine at 4 kHz through +1: about +6 dB (two octaves above the pivot).
        let sr = 48_000.0f32;
        let t = Tilt::new(1.0, sr);
        let mut st = TiltState::default();
        let mut peak = 0.0f32;
        for i in 0..48_000 {
            let y = st.tick(&t, math::sin(core::f32::consts::TAU * 4000.0 * i as f32 / sr));
            if i > 24_000 {
                peak = peak.max(y.abs());
            }
        }
        let db = 20.0 * math::log10(peak);
        assert!((db - 6.0).abs() < 0.6, "{db}");
    }
}
