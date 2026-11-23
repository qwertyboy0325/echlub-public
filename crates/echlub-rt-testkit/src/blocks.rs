//! Slicing a render into blocks, as an audio callback would.

/// A buffer of `frames` frames of `channels` interleaved channels, filled
/// by calling `process` on consecutive slices of `block` frames (the last
/// one shorter if `frames` is not a multiple). `process` is typically
/// `|out| engine.process(out)`.
pub fn in_blocks(frames: usize, block: usize, channels: usize, mut process: impl FnMut(&mut [f32])) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * channels];
    for chunk in out.chunks_mut(block.max(1) * channels) {
        process(chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn slices_cover_the_buffer_in_order() {
        let mut sizes = Vec::new();
        let mut k = 0.0;
        let out = super::in_blocks(10, 4, 2, |c| {
            sizes.push(c.len());
            for v in c.iter_mut() {
                k += 1.0;
                *v = k;
            }
        });
        assert_eq!(sizes, vec![8, 8, 4]);
        assert_eq!(out, (1..=20).map(|i| i as f32).collect::<Vec<_>>());
    }
}
