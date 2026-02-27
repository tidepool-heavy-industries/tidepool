use std::path::Path;
use tidepool_repr::Literal;
use tidepool_runtime::{compile_and_run_pure, compile_haskell, Value};

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Run compile_and_run_pure on a larger stack with Prelude includes.
fn run(src: &str, target: &str) -> tidepool_runtime::EvalResult {
    let pp = prelude_path();
    let src = src.to_owned();
    let target = target.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            compile_and_run_pure(&src, &target, &include).unwrap()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn test_compile_haskell_identity() {
    let pp = prelude_path();
    let src = "module Test where\nidentity :: a -> a\nidentity x = x";
    let (expr, table) = compile_haskell(src, "identity", &[pp.as_path()]).unwrap();
    assert!(!expr.nodes.is_empty());
    assert!(!table.is_empty());
}

#[test]
fn test_compile_and_run_literal() {
    let src = "module Test where\nfortyTwo :: Int\nfortyTwo = 42";
    let val = run(src, "fortyTwo").into_value();
    match val {
        Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 42),
        Value::Con(_, ref fields) => match fields.first() {
            Some(Value::Lit(Literal::LitInt(n))) => assert_eq!(*n, 42),
            other => panic!("unexpected boxed int field: {:?}", other),
        },
        other => panic!("expected int literal or boxed int, got: {:?}", other),
    }
}

#[test]
fn test_compile_and_run_arithmetic() {
    let src = "module Test where\nresult :: Int\nresult = 2 + 3";
    let val = run(src, "result").into_value();
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
fn test_compile_error() {
    let pp = prelude_path();
    let src = "module Test where\nbad = undefined_thing";
    let result = compile_haskell(src, "bad", &[pp.as_path()]);
    assert!(result.is_err());
}

#[test]
fn test_caching_produces_same_result() {
    let src = "module Test where\nval :: Int\nval = 10";
    let r1 = run(src, "val");
    let r2 = run(src, "val");
    assert_eq!(r1.to_string_pretty(), r2.to_string_pretty());
}

#[test]
fn test_eval_result_to_json() {
    let src = "module Test where\n\
               fortyTwo :: Int\n\
               fortyTwo = 42\n\
               \n\
               helloStr :: [Char]\n\
               helloStr = \"hello\"\n\
               \n\
               emptyIntList :: [Int]\n\
               emptyIntList = []\n\
               \n\
               intList :: [Int]\n\
               intList = [1, 2, 3]\n\
               \n\
               tupleVal :: (Int, Bool)\n\
               tupleVal = (1, True)\n\
               \n\
               boolVal :: Bool\n\
               boolVal = False\n\
               \n\
               maybeJust :: Maybe Int\n\
               maybeJust = Just 5\n\
               \n\
               maybeNothing :: Maybe Int\n\
               maybeNothing = Nothing";

    assert_eq!(run(src, "fortyTwo").to_json(), serde_json::json!(42));
    assert_eq!(run(src, "helloStr").to_json(), serde_json::json!("hello"));
    assert_eq!(run(src, "emptyIntList").to_json(), serde_json::json!([]));
    assert_eq!(run(src, "intList").to_json(), serde_json::json!([1, 2, 3]));

    match run(src, "tupleVal").to_json() {
        serde_json::Value::Array(ref arr) if arr.len() == 2 => {}
        other => panic!("unexpected JSON for tuple: {:?}", other),
    }

    assert!(run(src, "boolVal").to_json().is_boolean());
    let _ = run(src, "maybeJust").to_json();
    let _ = run(src, "maybeNothing").to_json();
}
