use tidepool_macro::haskell_eval;
use tidepool_eval::Value;

#[test]
fn test_haskell_eval_identity() {
    let res = haskell_eval!("../../haskell/test/Identity_cbor/identity.cbor");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.as_ref().err());
    let val = res.unwrap();
    // identity = \x -> x. In core-eval, Value::Closure is returned for lambdas.
    assert!(matches!(val, Value::Closure(_, _, _)));
}

#[test]
fn test_haskell_eval_apply() {
    let res = haskell_eval!("../../haskell/test/Identity_cbor/apply.cbor");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.as_ref().err());
    let val = res.unwrap();
    assert!(matches!(val, Value::Closure(_, _, _)));
}

#[test]
fn test_haskell_eval_const_prime() {
    let res = haskell_eval!("../../haskell/test/Identity_cbor/const'.cbor");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.as_ref().err());
    let val = res.unwrap();
    assert!(matches!(val, Value::Closure(_, _, _)));
}

#[test]
#[should_panic(expected = "failed to deserialize CBOR")]
fn test_haskell_eval_invalid_cbor() {
    // This should panic because the file is empty/invalid CBOR
    let _ = haskell_eval!("../../haskell/test/invalid.cbor");
}
