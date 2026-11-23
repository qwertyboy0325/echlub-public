//! Per-call cost of `echlub_dsp::math` beside the platform's std, f32 and
//! f64, on this machine (to compare implementations with
//! numbers, not from the totals of whole effects). Each row: the best of 7 passes over
//! 2^20 inputs in the range the DSP uses, summed so nothing is optimised
//! away; one core, scalar loop. Also the per-sample formulations the
//! effects use (`20 log10 x`, `10^(dB/20)`).
//!
//!   cargo run --release -p echlub-dsp --example math_cost

use echlub_dsp::math;
use std::hint::black_box as bb;
use std::time::Instant;

const N: usize = 1 << 20;

fn grid(lo: f64, hi: f64) -> Vec<f64> {
    (0..N).map(|i| lo + (hi - lo) * i as f64 / N as f64).collect()
}

fn ns<T: Copy>(xs: &[T], f: impl Fn(T) -> f64) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..7 {
        let t = Instant::now();
        let mut acc = 0.0f64;
        for &x in xs {
            acc += f(bb(x));
        }
        bb(acc);
        best = best.min(t.elapsed().as_secs_f64() * 1e9 / xs.len() as f64);
    }
    best
}

fn row(name: &str, xs: &[f64], wrap32: fn(f32) -> f32, std32: fn(f32) -> f32, wrap64: fn(f64) -> f64, std64: fn(f64) -> f64) {
    let x32: Vec<f32> = xs.iter().map(|&x| x as f32).collect();
    println!(
        "{name:28} {:7.2} {:7.2}   {:7.2} {:7.2}",
        ns(&x32, |x| wrap32(x) as f64),
        ns(&x32, |x| std32(x) as f64),
        ns(xs, wrap64),
        ns(xs, std64)
    );
}

fn main() {
    println!("{} {}, ns per call (best of 7 x 2^20)", std::env::consts::ARCH, std::env::consts::OS);
    println!("{:28} {:>7} {:>7}   {:>7} {:>7}", "function (inputs)", "f32 lib", "f32 std", "f64 lib", "f64 std");
    let tau = std::f64::consts::TAU;
    row("sin (±4 turns)", &grid(-2.0 * tau, 2.0 * tau), math::sin, f32::sin, math::sin, f64::sin);
    row("cos (±4 turns)", &grid(-2.0 * tau, 2.0 * tau), math::cos, f32::cos, math::cos, f64::cos);
    row("tan (±1.5)", &grid(-1.5, 1.5), math::tan, f32::tan, math::tan, f64::tan);
    row("atan (±50)", &grid(-50.0, 50.0), math::atan, f32::atan, math::atan, f64::atan);
    row("exp (-20..20)", &grid(-20.0, 20.0), math::exp, f32::exp, math::exp, f64::exp);
    row("exp2 (-20..20)", &grid(-20.0, 20.0), math::exp2, f32::exp2, math::exp2, f64::exp2);
    row("ln (1e-6..1)", &grid(1e-6, 1.0), math::ln, f32::ln, math::ln, f64::ln);
    row("log2 (1e-6..1)", &grid(1e-6, 1.0), math::log2, f32::log2, math::log2, f64::log2);
    row("log10 (1e-6..1)", &grid(1e-6, 1.0), math::log10, f32::log10, math::log10, f64::log10);
    row("tanh (±6)", &grid(-6.0, 6.0), math::tanh, f32::tanh, math::tanh, f64::tanh);
    row("pow10 (-6..1.2)", &grid(-6.0, 1.2), math::pow10, |x| 10f32.powf(x), math::pow10, |x| 10f64.powf(x));
    row("powf (x^1.7, 1e-3..10)", &grid(1e-3, 10.0), |x| math::powf(x, 1.7), |x| x.powf(1.7), |x| math::powf(x, 1.7), |x| x.powf(1.7));
    println!();
    println!("per-sample formulations (f32)");
    let db: Vec<f32> = grid(-60.0, 12.0).iter().map(|&x| x as f32).collect();
    let lv: Vec<f32> = grid(1e-6, 1.0).iter().map(|&x| x as f32).collect();
    let k2 = 20.0 * std::f32::consts::LOG10_2;
    for (name, v) in [
        ("from_db(dB) (now)", ns(&db, |x| math::from_db(x) as f64)),
        ("pow10(dB / 20)", ns(&db, |x| math::pow10(x / 20.0) as f64)),
        ("std 10^(dB / 20)", ns(&db, |x| 10f32.powf(x / 20.0) as f64)),
        ("20 log10 x (now)", ns(&lv, |x| (20.0 * math::log10(x)) as f64)),
        ("(20 log10 2) log2 x", ns(&lv, |x| (k2 * math::log2(x)) as f64)),
        ("std 20 log10 x", ns(&lv, |x| (20.0 * x.log10()) as f64)),
    ] {
        println!("{name:28} {v:7.2}");
    }
}
