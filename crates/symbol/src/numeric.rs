//! Conversions between integers and floats, each with its rounding stated.

/// The nearest `f64` to `value`, built from two lossless `u32` halves.
///
/// Exact up to 2^53; above that it rounds to nearest, like any `u64` to `f64`
/// conversion, but the rounding happens once, in `mul_add`, rather than inside
/// an unannotated cast.
pub fn u64_to_f64(value: u64) -> f64 {
    const U32_RADIX: f64 = u32::MAX as f64 + 1.0;

    let high = u32::try_from(value >> u32::BITS).expect("upper half fits in u32");
    let low = u32::try_from(value & u64::from(u32::MAX)).expect("lower half fits in u32");
    f64::from(high).mul_add(U32_RADIX, f64::from(low))
}

/// `value` rounded to the nearest integer and kept within `low..=high`.
///
/// The one float-to-integer cast in the crate. Rust defines it fully: it
/// saturates at `0` and `u64::MAX` and maps NaN to `0`, so nothing truncates
/// or wraps; the clamp then pins the result to the caller's range, which is
/// where a computed value belongs even if rounding nudged it out.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "saturating by definition, then clamped to low..=high"
)]
pub fn round_to_u64_within(value: f64, low: u64, high: u64) -> u64 {
    (value.round() as u64).clamp(low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactness is the property under test, so compare bit patterns.
    fn exactly(value: u64, expected: f64) {
        assert_eq!(u64_to_f64(value).to_bits(), expected.to_bits(), "{value}");
    }

    #[test]
    fn u64_to_f64_is_exact_where_f64_can_be() {
        exactly(0, 0.0);
        exactly(1 << 40, 1_099_511_627_776.0);
        exactly((1 << 53) - 1, 9_007_199_254_740_991.0);
        exactly(u64::MAX, 18_446_744_073_709_551_616.0);
    }

    #[test]
    fn rounding_stays_in_range_whatever_the_input() {
        assert_eq!(round_to_u64_within(2.5, 0, 10), 3);
        assert_eq!(round_to_u64_within(2.4, 0, 10), 2);
        assert_eq!(round_to_u64_within(-7.0, 3, 10), 3);
        assert_eq!(round_to_u64_within(1e30, 3, 10), 10);
        assert_eq!(round_to_u64_within(f64::NAN, 3, 10), 3);
        assert_eq!(round_to_u64_within(f64::INFINITY, 3, 10), 10);
    }
}
