use tidepool_repr::Literal;
use tidepool_runtime::Value;
use tidepool_testing::eval_harness::EvalHarness;

/// The stdlib-included harness these tests share.
fn harness() -> EvalHarness {
    EvalHarness::new().with_stdlib()
}

/// Extract an `Int` from either a raw `LitInt` or a boxed `I#` constructor.
fn expect_int(val: &Value) -> i64 {
    match val {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, ref fields) => match fields.first() {
            Some(Value::Lit(Literal::LitInt(n))) => *n,
            other => panic!("unexpected boxed int field: {:?}", other),
        },
        other => panic!("expected int literal or boxed int, got: {:?}", other),
    }
}

/// Shared module for `integration_family`: `identity` (compile-only shape
/// check), `arithResult` (compile+run arithmetic), and every binding
/// `test_eval_result_to_json` used to compile separately (`fortyTwo` doubles
/// as the old `test_compile_and_run_literal` fixture — same source, same
/// target name).
const FAMILY_SRC: &str = "module Test where\n\
    identity :: a -> a\n\
    identity x = x\n\
    \n\
    arithResult :: Int\n\
    arithResult = 2 + 3\n\
    \n\
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

const FAMILY_TARGETS: &[&str] = &[
    "identity",
    "arithResult",
    "fortyTwo",
    "helloStr",
    "emptyIntList",
    "intList",
    "tupleVal",
    "boolVal",
    "maybeJust",
    "maybeNothing",
];

/// Absorbs: test_compile_haskell_identity, test_compile_and_run_literal,
/// test_compile_and_run_arithmetic, test_eval_result_to_json — one compile
/// spawn, one shared module, every target run/inspected independently.
#[test]
fn integration_family() {
    let h = harness();
    let artifacts = h
        .compile_many(FAMILY_SRC, FAMILY_TARGETS)
        .expect("compile integration family module");

    // test_compile_haskell_identity: compile-only shape check, no JIT run.
    assert!(!artifacts.table.is_empty());
    let identity = artifacts.targets.get("identity").expect("identity target");
    assert!(!identity.expr.nodes.is_empty());

    // test_compile_and_run_arithmetic
    let arith = h.run_target_pure(&artifacts, "arithResult");
    assert_eq!(expect_int(arith.value()), 5);

    // test_compile_and_run_literal (fortyTwo)
    let forty_two = h.run_target_pure(&artifacts, "fortyTwo");
    assert_eq!(expect_int(forty_two.value()), 42);

    // test_eval_result_to_json
    assert_eq!(
        h.run_target_pure(&artifacts, "fortyTwo").json(),
        serde_json::json!(42)
    );
    assert_eq!(
        h.run_target_pure(&artifacts, "helloStr").json(),
        serde_json::json!("hello")
    );
    assert_eq!(
        h.run_target_pure(&artifacts, "emptyIntList").json(),
        serde_json::json!([])
    );
    assert_eq!(
        h.run_target_pure(&artifacts, "intList").json(),
        serde_json::json!([1, 2, 3])
    );

    match h.run_target_pure(&artifacts, "tupleVal").json() {
        serde_json::Value::Array(ref arr) if arr.len() == 2 => {}
        other => panic!("unexpected JSON for tuple: {:?}", other),
    }

    assert!(h.run_target_pure(&artifacts, "boolVal").json().is_boolean());
    let _ = h.run_target_pure(&artifacts, "maybeJust").json();
    let _ = h.run_target_pure(&artifacts, "maybeNothing").json();
}

// --- Standalone: distinct-mechanism / compile-fail probes ------------------

#[test]
fn test_compile_error() {
    let src = "module Test where\nbad = undefined_thing";
    let result = harness().compile(src, "bad");
    assert!(result.is_err());
}

/// Distinct-mechanism probe: two independent compiles of identical source
/// must produce byte-identical rendered output (compile-memo hit/miss
/// consistency) — the whole point is running the compile pipeline TWICE, so
/// this stays its own spawn (bundling it would defeat the property).
#[test]
fn test_caching_produces_same_result() {
    let src = "module Test where\nval :: Int\nval = 10";
    let r1 = harness().run_pure(src, "val").unwrap();
    let r2 = harness().run_pure(src, "val").unwrap();
    assert_eq!(r1.to_string_pretty(), r2.to_string_pretty());
}
