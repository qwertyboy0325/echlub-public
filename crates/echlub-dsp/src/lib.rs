//! Signal-processing building blocks for the EchLub engine: decibels, constant-power pan, a glide that
//! settles exactly, peak, cubic resampling, a radix-2 FFT. Everything that runs per block
//! takes buffers the caller owns and never allocates; whatever allocates
//! (resampling a file on import) says so and is meant for a worker thread.
//! Synthesis is not here: this crate holds only what every sound source shares.
//!
//! Platform-neutral: no I/O, no clock, no threads; builds for
//! `wasm32-unknown-unknown`. [`math`] holds the transcendental functions
//! every DAW crate uses (portable: the same bits on every target and
//! build profile).

pub mod fft;
pub mod math;

/// Decibels to a linear gain; at or below -120 dB it is silence.
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        math::pow10(db / 20.0)
    }
}

/// Constant-power pan law: `pan` -1 (left) .. 1 (right) to (left, right)
/// gains, each 1/sqrt(2) (-3 dB) in the centre.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (math::cos(a), math::sin(a))
}

/// A parameter that glides to its target (one-pole), so gain and pan
/// changes do not click. About 99% of the way in `ms`.
#[derive(Debug, Clone, Copy)]
pub struct Smoothed {
    target: f32,
    /// Distance still to go (value = target + diff). Kept apart so it
    /// decays to exactly zero: `target + (value - target) * coef` stalls a
    /// few float steps short of the target.
    diff: f32,
    coef: f32,
}

impl Smoothed {
    pub fn new(value: f32, sample_rate: u32, ms: f32) -> Self {
        let n = (ms / 1000.0 * sample_rate as f32).max(1.0);
        Self {
            target: value,
            diff: 0.0,
            coef: math::exp(-4.6 / n),
        }
    }
    pub fn set(&mut self, target: f32) {
        self.diff += self.target - target;
        self.target = target;
    }
    /// Jump to the target at once (a new graph, a seek while stopped).
    pub fn snap(&mut self) {
        self.diff = 0.0;
    }
    pub fn target(&self) -> f32 {
        self.target
    }
    /// Where it is now.
    pub fn value(&self) -> f32 {
        self.target + self.diff
    }
    /// Be at `value` now, gliding on to the same target (automation hands
    /// a level back to the fader).
    pub fn jump_to(&mut self, value: f32) {
        self.diff = value - self.target;
    }
    #[inline]
    pub fn tick(&mut self) -> f32 {
        self.diff *= self.coef;
        if self.diff.abs() < 1e-7 {
            self.diff = 0.0;
        }
        self.target + self.diff
    }
    pub fn settled(&self) -> bool {
        self.diff == 0.0
    }
    /// A settled glide over a block of `frames` (at least 1): the one value
    /// each of `frames` calls of [`Smoothed::tick`] would return, leaving
    /// the state as they would (`None` if it is not settled, or `frames`
    /// is 0: use `tick`). A settled `tick` returns exactly `target` (made
    /// `+0.0` if it was `-0.0`: the sum with the zero `diff`) and leaves
    /// `diff` at `+0.0`; this does the same once, so a loop over the block
    /// may use the value as a constant (and vectorise) with the same bits.
    #[inline]
    pub fn hold(&mut self, frames: usize) -> Option<f32> {
        if frames == 0 || self.diff != 0.0 {
            return None;
        }
        // What the first tick does to a zero `diff` of either sign.
        self.diff = 0.0;
        Some(self.target + self.diff)
    }
}

/// Peak of a block, for meters.
pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Resample `x` from `from` Hz to `to` Hz (cubic Hermite). Allocates: for
/// importing files off the audio thread, not for live use.
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let n = ((x.len() as u64 * to as u64) / from as u64) as usize;
    let step = from as f64 / to as f64;
    let at = |i: isize| x[i.clamp(0, x.len() as isize - 1) as usize];
    (0..n)
        .map(|k| {
            let t = k as f64 * step;
            let i = t.floor() as isize;
            let f = (t - i as f64) as f32;
            let (y0, y1, y2, y3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            let c1 = 0.5 * (y2 - y0);
            let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
            let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
            ((c3 * f + c2) * f + c1) * f + y1
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_is_constant_power() {
        for p in [-1.0, -0.5, 0.0, 0.3, 1.0] {
            let (l, r) = pan_gains(p);
            assert!((l * l + r * r - 1.0).abs() < 1e-6);
        }
        let (l, r) = pan_gains(-1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
    }

    #[test]
    fn smoothing_reaches_its_target_without_a_jump() {
        let mut s = Smoothed::new(0.0, 48_000, 10.0);
        s.set(1.0);
        let first = s.tick();
        assert!(first > 0.0 && first < 0.01, "{first}");
        for _ in 0..4800 {
            s.tick();
        }
        assert!(s.settled());
    }

    /// `hold` is `frames` ticks of a settled glide, bit for bit: the value
    /// of every tick, the state left behind, and what a later `set` or
    /// `jump_to` makes of it; for targets of every kind (signed zeros,
    /// denormals, infinities, NaN) and a `diff` of either zero.
    #[test]
    fn hold_is_what_ticks_of_a_settled_glide_return() {
        let targets = [0.0f32, -0.0, 1.0, -1.0, 0.5, 1e-40, -1e-40, f32::MIN_POSITIVE, 1e30, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 0.123_456_78];
        for &target in &targets {
            for diff in [0.0f32, -0.0] {
                for frames in [1usize, 2, 17, 128] {
                    let mut a = Smoothed::new(target, 48_000, 10.0);
                    a.diff = diff;
                    let mut b = a;
                    let v = a.hold(frames).expect("settled");
                    for _ in 0..frames {
                        assert_eq!(b.tick().to_bits(), v.to_bits(), "target {target:?} diff {diff:?}");
                    }
                    assert_eq!(a.diff.to_bits(), b.diff.to_bits());
                    assert_eq!(a.value().to_bits(), b.value().to_bits());
                    assert!(a.settled() && b.settled());
                    for next in [0.7f32, -0.0, 0.0, target] {
                        let (mut x, mut y) = (a, b);
                        x.set(next);
                        y.set(next);
                        assert_eq!((x.diff.to_bits(), x.target.to_bits()), (y.diff.to_bits(), y.target.to_bits()));
                        x.jump_to(0.25);
                        y.jump_to(0.25);
                        assert_eq!(x.diff.to_bits(), y.diff.to_bits());
                    }
                }
            }
        }
    }

    #[test]
    fn hold_declines_a_glide_in_motion_and_an_empty_block() {
        let mut s = Smoothed::new(0.0, 48_000, 10.0);
        assert!(s.hold(0).is_none());
        s.set(1.0);
        assert!(s.hold(64).is_none());
        // Gliding on: its ticks are untouched by the refused hold.
        let mut t = s;
        assert_eq!(s.tick().to_bits(), t.tick().to_bits());
        for _ in 0..48_000 {
            s.tick();
        }
        assert_eq!(s.hold(1), Some(1.0));
    }

    #[test]
    fn resampling_keeps_a_sine_and_its_length() {
        let x: Vec<f32> = (0..44_100).map(|i| math::sin(i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0)).collect();
        let y = resample(&x, 44_100, 48_000);
        assert_eq!(y.len(), 48_000);
        let worst = (100..47_900)
            .map(|i| (y[i] - math::sin(i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0)).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "{worst}");
    }
}
