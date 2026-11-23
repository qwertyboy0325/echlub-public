//! Bit-for-bit comparison of sample buffers.
//!
//! An identity claim ("offline equals live", "any block size gives the
//! same samples") means the same bits, not values that compare equal as
//! floats: `==` on `f32` treats `-0.0` and `0.0` as equal and `NaN` as
//! unequal to itself. These helpers compare `to_bits()` and, on a
//! mismatch, report the first differing index and both values, which is
//! what one needs to find the cause.

/// The first index where `a` and `b` differ in bits, or where one ends
/// before the other; `None` if they are identical.
pub fn first_difference(a: &[f32], b: &[f32]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x.to_bits() != y.to_bits()).or(if a.len() == b.len() { None } else { Some(a.len().min(b.len())) })
}

/// Same length and the same bits everywhere.
pub fn same_bits(a: &[f32], b: &[f32]) -> bool {
    first_difference(a, b).is_none()
}

/// Panic unless `a` and `b` are bit-identical, naming `what` and the first
/// differing index (and, for interleaved data of `channels` channels, the
/// frame).
#[track_caller]
pub fn assert_same_bits_interleaved(a: &[f32], b: &[f32], channels: usize, what: impl std::fmt::Display) {
    if let Some(i) = first_difference(a, b) {
        let show = |v: &[f32]| v.get(i).map_or_else(|| "(ended)".to_string(), |x| format!("{x:e} ({:#010x})", x.to_bits()));
        panic!(
            "{what}: not bit-identical; first difference at index {i} (frame {}), {} vs {}; lengths {} and {}",
            i / channels.max(1),
            show(a),
            show(b),
            a.len(),
            b.len()
        );
    }
}

/// [`assert_same_bits_interleaved`] for interleaved stereo.
#[track_caller]
pub fn assert_same_bits(a: &[f32], b: &[f32], what: impl std::fmt::Display) {
    assert_same_bits_interleaved(a, b, 2, what);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differences_are_found_by_bits_and_length() {
        assert_eq!(first_difference(&[1.0, 2.0], &[1.0, 2.0]), None);
        assert_eq!(first_difference(&[0.0, 0.0], &[0.0, -0.0]), Some(1), "signed zeros differ in bits");
        assert_eq!(first_difference(&[f32::NAN], &[f32::NAN]), None, "the same NaN is the same bits");
        assert_eq!(first_difference(&[1.0, 2.0], &[1.0]), Some(1), "a shorter buffer differs where it ends");
        assert!(same_bits(&[], &[]));
    }

    #[test]
    #[should_panic(expected = "first difference at index 3 (frame 1)")]
    fn a_mismatch_names_the_index_and_frame() {
        assert_same_bits(&[0.0, 0.0, 0.0, 1.0], &[0.0, 0.0, 0.0, 2.0], "case");
    }
}
