use tidepool_macro::haskell_eval;
use core_eval::Value;

#[test]
fn test_haskell_eval_identity() {
    let res = haskell_eval!("../../haskell/test/Identity_cbor/identity.cbor");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.err());
    let val = res.unwrap();
    // identity = \x -> x. In core-eval, Value::Closure is returned for lambdas.
    assert!(matches!(val, Value::Closure(_, _, _)));
}

#[test]
fn test_haskell_eval_apply() {
    let res = haskell_eval!("../../haskell/test/Identity_cbor/apply.cbor");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.err());
    let val = res.unwrap();
    assert!(matches!(val, Value::Closure(_, _, _)));
}

#[test]
fn test_haskell_eval_const_prime() {
    let res = haskell_eval!("../../haskell/test/Identity_cbor/const'.cbor");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.err());
    let val = res.unwrap();
    assert!(matches!(val, Value::Closure(_, _, _)));
}
