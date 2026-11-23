//! A radix-2 complex FFT in f64, ours (no library): twiddles from
//! [`crate::math`], so the same bits on every platform. Moved here from
//! the time-stretch crate so that other crates (a synthesizer, say) can use it too.

use crate::math;

const TAU: f64 = std::f64::consts::TAU;

/// The tables for one power-of-two length.
#[derive(Debug, Clone)]
pub struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    rev: Vec<u32>,
}

impl Fft {
    /// An FFT of `n` points (a power of two, at least 2).
    pub fn new(n: usize) -> Fft {
        assert!(n >= 2 && n.is_power_of_two(), "FFT length {n} is not a power of two");
        let (sin, cos): (Vec<f64>, Vec<f64>) = (0..n / 2).map(|k| math::sin_cos(TAU * k as f64 / n as f64)).unzip();
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        Fft { n, cos, sin, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// In place: forward (`X[k] = sum x[t] e^(-2 pi i k t / n)`) or inverse
    /// (the conjugate exponent, without the 1/n).
    pub fn run(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.n;
        assert!(re.len() == n && im.len() == n);
        for i in 0..n {
            let j = self.rev[i] as usize;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            let half = len / 2;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let c = self.cos[k * step];
                    let s = if inverse { self.sin[k * step] } else { -self.sin[k * step] };
                    let (a, b) = (start + k, start + k + half);
                    let xr = re[b] * c - im[b] * s;
                    let xi = re[b] * s + im[b] * c;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_and_a_tone_on_its_bin() {
        let n = 64;
        let f = Fft::new(n);
        let x: Vec<f64> = (0..n).map(|t| math::cos(TAU * 5.0 * t as f64 / n as f64) + 0.25 * t as f64 / n as f64).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        f.run(&mut re, &mut im, false);
        // The cosine: n/2 on bins 5 and n-5 (plus the ramp's spectrum).
        let ramp: Vec<f64> = (0..n).map(|t| 0.25 * t as f64 / n as f64).collect();
        let (mut rr, mut ri) = (ramp, vec![0.0; n]);
        f.run(&mut rr, &mut ri, false);
        assert!((re[5] - rr[5] - n as f64 / 2.0).abs() < 1e-9 && (im[5] - ri[5]).abs() < 1e-9);
        f.run(&mut re, &mut im, true);
        for t in 0..n {
            assert!((re[t] / n as f64 - x[t]).abs() < 1e-12 && (im[t] / n as f64).abs() < 1e-12);
        }
    }
}
