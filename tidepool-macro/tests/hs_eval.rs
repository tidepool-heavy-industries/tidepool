use tidepool_macro::haskell_eval;
use tidepool_eval::Value;

// .hs paths resolve relative to CARGO_MANIFEST_DIR (tidepool-macro/),
// so one ../ reaches the workspace root.

#[test]
fn test_hs_identity() {
    let res = haskell_eval!("../haskell/test/Identity.hs::identity");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.as_ref().err());
    assert!(matches!(res.unwrap(), Value::Closure(_, _, _)));
}

#[test]
fn test_hs_apply() {
    let res = haskell_eval!("../haskell/test/Identity.hs::apply");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.as_ref().err());
    assert!(matches!(res.unwrap(), Value::Closure(_, _, _)));
}

#[test]
fn test_hs_const_prime() {
    let res = haskell_eval!("../haskell/test/Identity.hs::const'");
    assert!(res.is_ok(), "Evaluation failed: {:?}", res.as_ref().err());
    assert!(matches!(res.unwrap(), Value::Closure(_, _, _)));
}
