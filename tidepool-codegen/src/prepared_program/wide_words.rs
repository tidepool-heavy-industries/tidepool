//! Exact native-word operations: wide results remain two Word64 values, never
//! an admitted 128-bit schema representation. Scalar results are high/low for
//! add/multiply, quotient/remainder for division. Division is noncollecting;
//! its host wrapper must record failure before publishing either result slot.

use crate::host_fns::RuntimeError;

/// GHC requires high < divisor. Reject undefined inputs without a native trap
/// or truncated quotient; all arithmetic here is internal Rust, not the ABI.
pub(super) fn checked_quot_rem(high: u64, low: u64, divisor: u64) -> Result<(u64, u64), RuntimeError> {
    if divisor == 0 {
        return Err(RuntimeError::DivisionByZero);
    }
    if high >= divisor {
        return Err(RuntimeError::Overflow);
    }
    let numerator = (u128::from(high) << 64) | u128::from(low);
    Ok(((numerator / u128::from(divisor)) as u64, (numerator % u128::from(divisor)) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_word_division_domain_and_result_order() {
        assert_eq!(checked_quot_rem(1, 3, 2), Ok((1 << 63 | 1, 1)));
        assert_eq!(checked_quot_rem(0, 42, 5), Ok((8, 2)));
        assert_eq!(checked_quot_rem(0, 1, 0), Err(RuntimeError::DivisionByZero));
        assert_eq!(checked_quot_rem(2, 0, 2), Err(RuntimeError::Overflow));
    }
}
