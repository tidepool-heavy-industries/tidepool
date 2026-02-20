use frunk::HNil;
use tidepool_runtime::{compile_haskell, compile_and_run, Value, EvalResult};
use tidepool_repr::Literal;

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
    let val = compile_and_run(src, "fortyTwo", &[], &mut HNil, &()).unwrap().into_value();
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
    let val = compile_and_run(src, "result", &[], &mut HNil, &()).unwrap().into_value();
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
    let r1 = compile_and_run(src, "val", &[], &mut HNil, &()).unwrap();
    let r2 = compile_and_run(src, "val", &[], &mut HNil, &()).unwrap();
    // Both should produce the same value (second from cache)
    assert_eq!(r1.to_string_pretty(), r2.to_string_pretty());
}

#[test]
#[ignore]
fn test_eval_result_to_json() {
    // Exercise JSON rendering for a variety of value shapes to catch regressions.
    let src = r#"module Test where
fortyTwo :: Int
fortyTwo = 42

helloStr :: String
helloStr = "héllø 🌍"

emptyIntList :: [Int]
emptyIntList = []

intList :: [Int]
intList = [1, 2, 3]

charList :: [Char]
charList = "abc"

tupleVal :: (Int, Bool)
tupleVal = (1, True)

unitVal :: ()
unitVal = ()

maybeJust :: Maybe Int
maybeJust = Just 5

maybeNothing :: Maybe Int
maybeNothing = Nothing

boolVal :: Bool
boolVal = False

nanVal :: Double
nanVal = 0/0

infVal :: Double
infVal = 1/0

customData :: Either Int String
customData = Right "ok"

deepList :: [Int]
deepList = [1..1000]
"#;

    // Simple integer case should still render as JSON number 42.
    let int_result = compile_and_run(src, "fortyTwo", &[], &mut HNil, &()).unwrap();
    let int_json = int_result.to_json();
    assert_eq!(int_json, serde_json::json!(42));

    // Strings (including UTF-8).
    let string_result = compile_and_run(src, "helloStr", &[], &mut HNil, &()).unwrap();
    let string_json = string_result.to_json();
    assert_eq!(string_json, serde_json::json!("héllø 🌍"));

    // Lists: empty list and a small numeric list.
    let empty_list_json =
        compile_and_run(src, "emptyIntList", &[], &mut HNil, &()).unwrap().to_json();
    assert_eq!(empty_list_json, serde_json::json!([]));

    let int_list_json =
        compile_and_run(src, "intList", &[], &mut HNil, &()).unwrap().to_json();
    assert_eq!(int_list_json, serde_json::json!([1, 2, 3]));

    // Character list: depending on implementation this might render as a string or an array.
    let char_list_json =
        compile_and_run(src, "charList", &[], &mut HNil, &()).unwrap().to_json();
    match char_list_json {
        serde_json::Value::String(_) | serde_json::Value::Array(_) => { /* acceptable */ }
        other => panic!("unexpected JSON for char list: {:?}", other),
    }

    // Tuples: commonly rendered as a JSON array of fixed length.
    let tuple_json =
        compile_and_run(src, "tupleVal", &[], &mut HNil, &()).unwrap().to_json();
    match tuple_json {
        serde_json::Value::Array(ref arr) if arr.len() == 2 => { /* expected arity */ }
        other => panic!("unexpected JSON for tuple: {:?}", other),
    }

    // Booleans should render as JSON booleans.
    let bool_json =
        compile_and_run(src, "boolVal", &[], &mut HNil, &()).unwrap().to_json();
    match bool_json {
        serde_json::Value::Bool(_) => { /* ok */ }
        other => panic!("unexpected JSON for Bool: {:?}", other),
    }

    // Unit, Maybe, custom data constructors, floats (NaN/Inf), and deep lists:
    // we don't assert a specific shape here, but we do ensure that JSON
    // rendering succeeds without panicking on these edge cases.
    let _unit_json =
        compile_and_run(src, "unitVal", &[], &mut HNil, &()).unwrap().to_json();
    let _maybe_just_json =
        compile_and_run(src, "maybeJust", &[], &mut HNil, &()).unwrap().to_json();
    let _maybe_nothing_json =
        compile_and_run(src, "maybeNothing", &[], &mut HNil, &()).unwrap().to_json();
    let _nan_json =
        compile_and_run(src, "nanVal", &[], &mut HNil, &()).unwrap().to_json();
    let _inf_json =
        compile_and_run(src, "infVal", &[], &mut HNil, &()).unwrap().to_json();
    let _custom_json =
        compile_and_run(src, "customData", &[], &mut HNil, &()).unwrap().to_json();
    let _deep_json =
        compile_and_run(src, "deepList", &[], &mut HNil, &()).unwrap().to_json();
}
