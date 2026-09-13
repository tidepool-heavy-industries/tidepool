//! W5_FORMATTING: registered compiler intrinsics, not dynamic symbol lookup.
//! The Haskell wrapper controls demand: inspect Double before forcing precedence,
//! and force precedence only for negative values or negative zero.
//! Bytes use the existing prepared external owner; construct Text in generated
//! code using the producer's actual constructor descriptor, never a Core bridge.

use tidepool_repr::execution_schema::{ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature};

#[derive(Clone, Copy)]
pub(super) enum FormattingOperation { Bytes, PrecBytes, NeedsPrecedence }

pub(super) fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<FormattingOperation> {
    use RuntimeRep::*;
    let OperationIdentity::Intrinsic { symbol, convention: ForeignConvention::CCall } = identity else { return None; };
    let (arguments, results, operation) = match symbol.as_str() {
        "prepared_render_double_bytes" => (vec![Float(64)], vec![UnliftedRef], FormattingOperation::Bytes),
        "prepared_render_double_prec_bytes" => (vec![Int(64), Float(64)], vec![UnliftedRef], FormattingOperation::PrecBytes),
        "prepared_double_needs_precedence" => (vec![Float(64)], vec![Int(64)], FormattingOperation::NeedsPrecedence),
        _ => return None,
    };
    (signature.arguments == arguments && signature.results == ResultContract::Returns(results)).then_some(operation)
}

/// GHC showSignedFloat includes negative zero but not negative NaN. The
/// generated predicate must implement this exact condition, not sign-bit alone.
fn needs_precedence(value: f64) -> bool {
    value < 0.0 || (value == 0.0 && value.is_sign_negative())
}

fn render(value: f64, precedence: i64) -> String {
    let body = tidepool_bignum::haskell_show_double(value);
    if precedence > 6 && needs_precedence(value) { format!("({body})") } else { body }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn w5_formatting_signed_zero_and_nan_follow_ghc_precedence() {
        assert_eq!(render(-0.0, 6), "-0.0");
        assert_eq!(render(-0.0, 7), "(-0.0)");
        assert_eq!(render(f64::NEG_INFINITY, 7), "(-Infinity)");
        assert_eq!(render(-f64::NAN, 7), "NaN");
        assert!(!needs_precedence(-f64::NAN));
        assert!(!needs_precedence(1.5));
    }
}
