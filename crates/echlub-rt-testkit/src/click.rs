//! The click metric: the largest absolute third difference of a signal,
//! `|x[n+3] - 3 x[n+2] + 3 x[n+1] - x[n]|`. A smooth signal (a sine, a
//! fade, a glide) has a small third difference; a step or a corner in the
//! waveform or its slope stands out by orders of magnitude, so one number
//! separates "continuous" from "clicks".

/// The click metric of one channel.
pub fn mono(x: &[f32]) -> f32 {
    x.windows(4).map(|w| (w[3] - 3.0 * w[2] + 3.0 * w[1] - w[0]).abs()).fold(0.0, f32::max)
}

/// The click metric of interleaved audio: the worst of its channels.
pub fn interleaved(x: &[f32], channels: usize) -> f32 {
    let mut worst = 0.0f32;
    for c in 0..channels {
        let ch: Vec<f32> = x.iter().skip(c).step_by(channels).copied().collect();
        worst = worst.max(mono(&ch));
    }
    worst
}

/// The click metric of interleaved stereo.
pub fn stereo(x: &[f32]) -> f32 {
    interleaved(x, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_clicks_and_a_sine_does_not() {
        let sine: Vec<f32> = (0..4_800).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin()).collect();
        let mut step = vec![0.0f32; 100];
        step[50..].fill(0.5);
        assert!(mono(&sine) < 0.01, "{}", mono(&sine));
        assert_eq!(mono(&step), 1.0, "a step of 0.5 has a third difference of twice its size");
        let both: Vec<f32> = sine.iter().zip(step.iter().cycle()).flat_map(|(a, b)| [*a, *b]).collect();
        assert_eq!(stereo(&both), 1.0, "the worst channel counts");
        assert_eq!(mono(&[1.0, 2.0]), 0.0, "too short to measure");
    }
}
