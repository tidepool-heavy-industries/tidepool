//! `expect_int` — standalone, no dependency on `session_scaffold.rs`'s `C1`.

/// Extract an `Int` from a pure-run result HaskellValue (Lit or 1-arg Con wrapping one).
pub fn expect_int(v: &tidepool_bridge::HaskellValue) -> i64 {
    use tidepool_bridge::HaskellValue;
    use tidepool_repr::types::Literal;
    match v {
        HaskellValue::Lit(Literal::LitInt(n)) => *n,
        HaskellValue::Con(_, fields) if fields.len() == 1 => expect_int(&fields[0]),
        other => panic!("expected an Int result, got {other:?}"),
    }
}
