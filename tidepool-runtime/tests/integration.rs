use frunk::HNil;
use tidepool_runtime::{compile_haskell, compile_and_run, Value};
use core_repr::Literal;

#[test]
#[ignore] // Manual test: requires tidepool-extract on PATH
fn test_compile_haskell_identity() {
    let src = "module Test where\nidentity :: a -> a\nidentity x = x";
    let (expr, table) = compile_haskell(src, "identity", &[]).unwrap();
    assert!(!expr.nodes.is_empty());
    assert!(!table.is_empty()); // just check it loaded
}

#[test]
#[ignore] // Manual test: requires tidepool-extract on PATH
fn test_compile_and_run_literal() {
    let src = "module Test where\nfortyTwo :: Int\nfortyTwo = 42";
    let val = compile_and_run(src, "fortyTwo", &[], &mut HNil, &()).unwrap();
    match val {
        Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 42),
        Value::Con(_, ref fields) => {
            // GHC may box as I# constructor
            match fields.first() {
                Some(Value::Lit(Literal::LitInt(n))) => assert_eq!(*n, 42),
                other => panic!("unexpected boxed int field: {:?}", other),
            }
        }
        other => panic!("expected int literal or boxed int, got: {:?}", other),
    }
}

#[test]
#[ignore] // Manual test: requires tidepool-extract on PATH
fn test_compile_and_run_arithmetic() {
    let src = "module Test where\nresult :: Int\nresult = 2 + 3";
    let val = compile_and_run(src, "result", &[], &mut HNil, &()).unwrap();
    // Result may be boxed I# 5 or literal 5
    match val {
        Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 5),
        Value::Con(_, ref fields) => match fields.first() {
            Some(Value::Lit(Literal::LitInt(n))) => assert_eq!(*n, 5),
            other => panic!("unexpected: {:?}", other),
        },
        other => panic!("expected 5, got: {:?}", other),
    }
}

#[test]
#[ignore] // Manual test: requires tidepool-extract on PATH
fn test_compile_error() {
    let src = "module Test where\nbad = undefined_thing";
    let result = compile_haskell(src, "bad", &[]);
    assert!(result.is_err());
}

#[test]
#[ignore] // Manual test: requires tidepool-extract on PATH
fn test_caching_produces_same_result() {
    let src = "module Test where\nval :: Int\nval = 10";
    let v1 = compile_and_run(src, "val", &[], &mut HNil, &()).unwrap();
    let v2 = compile_and_run(src, "val", &[], &mut HNil, &()).unwrap();
    // Both should produce the same value (second from cache)
    assert_eq!(format!("{:?}", v1), format!("{:?}", v2));
}
