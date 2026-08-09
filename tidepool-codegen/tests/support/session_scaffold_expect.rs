//! `expect_int` — standalone, no dependency on `session_scaffold.rs`'s `C1`.

/// Extract an `Int` from a pure-run result Value (Lit or 1-arg Con wrapping one).
pub fn expect_int(v: &tidepool_eval::value::Value) -> i64 {
    use tidepool_eval::value::Value;
    use tidepool_repr::types::Literal;
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if fields.len() == 1 => expect_int(&fields[0]),
        other => panic!("expected an Int result, got {other:?}"),
    }
}
