//! Portable math: the transcendental
//! functions every DAW audio and design path uses, the same bits on every
//! target and build profile.
//!
//! Why not std: `f32::sin` and friends call the platform's C math library
//! (glibc on Linux, Apple's libm on macOS), which differ by 1-2 ulp on some
//! inputs; and an optimising build may merge `cos(a)` and `sin(a)` into one
//! `sincos` library call that differs again (seen on macOS). Here every
//! function is the pure-Rust `libm` crate (a port of musl's math), which
//! uses hardware only for exactly rounded operations; its functions are
//! plain Rust, which LLVM neither merges nor folds as library calls. See
//! `docs/architecture/daw-cross-platform-identity.md`.
//!
//! Policy:
//! - Transcendentals (sin, cos, tan, atan, atan2, exp, exp2, ln, log2,
//!   log10, powf, tanh) only through this module; the workspace this crate
//!   comes from enforces that with a script that fails on a direct std call in
//!   the audio crates.
//! - [`pow10`] (`10^x`, decibels to gain) is `2^(x log2 10)` evaluated in
//!   f64, then rounded: faster than `powf(10, x)` and accurate (see the
//!   tests).
//! - [`powi`] is an explicit square-and-multiply loop here (an LLVM
//!   intrinsic could be expanded inline in one build and called in
//!   another).
//! - Exactly rounded operations stay std: `sqrt`, `abs`, `floor`, `ceil`,
//!   `round`, `trunc`, `fract`, `mul_add`, `%`, `rem_euclid`, `min`, `max`
//!   and casts (IEEE 754 requires one result for each).
//! - [`sin_cos`] is two separate calls, never a merged one.

/// f32 and f64: the functions this module offers.
pub trait Real: Copy {
    fn sin_(self) -> Self;
    fn cos_(self) -> Self;
    fn tan_(self) -> Self;
    fn atan_(self) -> Self;
    fn atan2_(self, x: Self) -> Self;
    fn exp_(self) -> Self;
    fn exp2_(self) -> Self;
    fn ln_(self) -> Self;
    fn log2_(self) -> Self;
    fn log10_(self) -> Self;
    fn powf_(self, e: Self) -> Self;
    fn tanh_(self) -> Self;
    fn pow10_(self) -> Self;
    fn one() -> Self;
    fn mul(self, o: Self) -> Self;
    fn div(self, o: Self) -> Self;
}

impl Real for f32 {
    #[inline]
    fn sin_(self) -> f32 {
        libm::sinf(self)
    }
    #[inline]
    fn cos_(self) -> f32 {
        libm::cosf(self)
    }
    #[inline]
    fn tan_(self) -> f32 {
        libm::tanf(self)
    }
    #[inline]
    fn atan_(self) -> f32 {
        libm::atanf(self)
    }
    #[inline]
    fn atan2_(self, x: f32) -> f32 {
        libm::atan2f(self, x)
    }
    #[inline]
    fn exp_(self) -> f32 {
        libm::expf(self)
    }
    #[inline]
    fn exp2_(self) -> f32 {
        libm::exp2f(self)
    }
    #[inline]
    fn ln_(self) -> f32 {
        libm::logf(self)
    }
    #[inline]
    fn log2_(self) -> f32 {
        libm::log2f(self)
    }
    #[inline]
    fn log10_(self) -> f32 {
        libm::log10f(self)
    }
    #[inline]
    fn powf_(self, e: f32) -> f32 {
        libm::powf(self, e)
    }
    #[inline]
    fn tanh_(self) -> f32 {
        libm::tanhf(self)
    }
    #[inline]
    fn pow10_(self) -> f32 {
        libm::exp2(self as f64 * core::f64::consts::LOG2_10) as f32
    }
    #[inline]
    fn one() -> f32 {
        1.0
    }
    #[inline]
    fn mul(self, o: f32) -> f32 {
        self * o
    }
    #[inline]
    fn div(self, o: f32) -> f32 {
        self / o
    }
}

impl Real for f64 {
    #[inline]
    fn sin_(self) -> f64 {
        libm::sin(self)
    }
    #[inline]
    fn cos_(self) -> f64 {
        libm::cos(self)
    }
    #[inline]
    fn tan_(self) -> f64 {
        libm::tan(self)
    }
    #[inline]
    fn atan_(self) -> f64 {
        libm::atan(self)
    }
    #[inline]
    fn atan2_(self, x: f64) -> f64 {
        libm::atan2(self, x)
    }
    #[inline]
    fn exp_(self) -> f64 {
        libm::exp(self)
    }
    #[inline]
    fn exp2_(self) -> f64 {
        libm::exp2(self)
    }
    #[inline]
    fn ln_(self) -> f64 {
        libm::log(self)
    }
    #[inline]
    fn log2_(self) -> f64 {
        libm::log2(self)
    }
    #[inline]
    fn log10_(self) -> f64 {
        libm::log10(self)
    }
    #[inline]
    fn powf_(self, e: f64) -> f64 {
        libm::pow(self, e)
    }
    #[inline]
    fn tanh_(self) -> f64 {
        libm::tanh(self)
    }
    /// f64 has no wider type here: `pow` itself (correct to about 1 ulp).
    #[inline]
    fn pow10_(self) -> f64 {
        libm::pow(10.0, self)
    }
    #[inline]
    fn one() -> f64 {
        1.0
    }
    #[inline]
    fn mul(self, o: f64) -> f64 {
        self * o
    }
    #[inline]
    fn div(self, o: f64) -> f64 {
        self / o
    }
}

#[inline]
pub fn sin<T: Real>(x: T) -> T {
    x.sin_()
}
#[inline]
pub fn cos<T: Real>(x: T) -> T {
    x.cos_()
}
/// `(sin x, cos x)` as two separate calls (never a merged `sincos`).
#[inline]
pub fn sin_cos<T: Real>(x: T) -> (T, T) {
    (x.sin_(), x.cos_())
}
#[inline]
pub fn tan<T: Real>(x: T) -> T {
    x.tan_()
}
#[inline]
pub fn atan<T: Real>(x: T) -> T {
    x.atan_()
}
/// The angle of `(x, y)`, as `y.atan2(x)`.
#[inline]
pub fn atan2<T: Real>(y: T, x: T) -> T {
    y.atan2_(x)
}
#[inline]
pub fn exp<T: Real>(x: T) -> T {
    x.exp_()
}
#[inline]
pub fn exp2<T: Real>(x: T) -> T {
    x.exp2_()
}
/// Natural logarithm.
#[inline]
pub fn ln<T: Real>(x: T) -> T {
    x.ln_()
}
#[inline]
pub fn log2<T: Real>(x: T) -> T {
    x.log2_()
}
#[inline]
pub fn log10<T: Real>(x: T) -> T {
    x.log10_()
}
/// `base^e`. For `10^x` use [`pow10`], for `2^x` [`exp2`].
#[inline]
pub fn powf<T: Real>(base: T, e: T) -> T {
    base.powf_(e)
}
#[inline]
pub fn tanh<T: Real>(x: T) -> T {
    x.tanh_()
}
/// `10^x` (f32: `2^(x log2 10)` in f64, rounded once).
#[inline]
pub fn pow10<T: Real>(x: T) -> T {
    x.pow10_()
}
/// Decibels to a gain, `10^(db/20)`, for per-sample use: `2^(db · log2(10)/20)`
/// with f32 `exp2` (4.0 ns against 6.9 for [`pow10`] here). Within 12 ulp of
/// `10^(db/20)` over -120..+24 dB, at most 6.6e-6 dB (tested); [`pow10`] of
/// `db / 20` is itself 8 ulp off, as the division rounds first.
#[inline]
pub fn from_db(db: f32) -> f32 {
    exp2(db * (core::f32::consts::LOG2_10 / 20.0))
}

/// `x^n` by square-and-multiply, the same operations in every build
/// (`1 / x^|n|` for negative `n`).
#[inline]
pub fn powi<T: Real>(x: T, n: i32) -> T {
    let mut base = x;
    let mut k = n.unsigned_abs();
    let mut r = T::one();
    loop {
        if k & 1 != 0 {
            r = r.mul(base);
        }
        k >>= 1;
        if k == 0 {
            break;
        }
        base = base.mul(base);
    }
    if n < 0 {
        T::one().div(r)
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ulps(a: f32, b: f32) -> u64 {
        if a.is_nan() && b.is_nan() {
            return 0;
        }
        let o = |v: f32| {
            let u = v.to_bits();
            if u >> 31 != 0 {
                -((u & 0x7fff_ffff) as i64)
            } else {
                u as i64
            }
        };
        (o(a) - o(b)).unsigned_abs()
    }

    /// `pow10` for decibel-sized arguments: within 1 ulp of 10^x computed
    /// in f64 (itself within 1 ulp of exact, far below an f32 ulp).
    #[test]
    fn pow10_is_within_one_ulp_over_the_decibel_range() {
        let mut worst = 0;
        for i in 0..=400_000 {
            let x = -10.0 + 16.0 * i as f32 / 400_000.0; // -200 .. +120 dB / 20
            let r = libm::pow(10.0, x as f64) as f32;
            worst = worst.max(ulps(pow10(x), r));
        }
        assert!(worst <= 1, "{worst} ulp");
    }

    /// `from_db` against `10^(db/20)` in f64: at most 12 ulp and 7e-6 dB
    /// over -120..+24 dB.
    #[test]
    fn from_db_is_within_twelve_ulp_and_7e_6_db() {
        let (mut worst_ulp, mut worst_db) = (0u64, 0f64);
        for i in 0..=1_440_000 {
            let db = -120.0 + i as f32 * 1e-4;
            let exact = libm::pow(10.0, db as f64 / 20.0);
            let v = from_db(db);
            worst_ulp = worst_ulp.max(ulps(v, exact as f32));
            worst_db = worst_db.max((20.0 * libm::log10(v as f64 / exact)).abs());
        }
        assert!(worst_ulp <= 12 && worst_db <= 7e-6, "{worst_ulp} ulp, {worst_db:e} dB");
    }

    #[test]
    fn powi_is_exact_where_it_can_be_and_handles_signs() {
        assert_eq!(powi(2.0f32, 10), 1024.0);
        assert_eq!(powi(2.0f64, -3), 0.125);
        assert_eq!(powi(-3.0f32, 3), -27.0);
        assert_eq!(powi(7.5f64, 0), 1.0);
        assert_eq!(powi(1.1f64, 2), 1.1 * 1.1);
    }

    #[test]
    fn sin_cos_is_the_two_functions() {
        for i in 0..1000 {
            let x = i as f32 * 0.0131 - 6.0;
            assert_eq!(sin_cos(x), (sin(x), cos(x)));
        }
    }
}
