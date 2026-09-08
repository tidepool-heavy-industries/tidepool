//! GHC-backed regressions from the engine review. These assert intended
//! semantics; failures identify engine defects, not expected-error successes.

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::Literal;
use tidepool_testing::eval_harness::{require_extract, with_eval_stack, EvalHarness};

const SOURCE: &str = include_str!("fixtures/EngineReview.hs");

fn ghc_answer(target: &str) -> i64 {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/EngineReview.hs");
    let output = Command::new("ghc")
        .arg("-ignore-dot-ghci")
        .arg(fixture)
        .args(["-e", &format!("print EngineReview.{target}")])
        .output()
        .expect("run GHC oracle in the repository Nix environment");
    assert!(
        output.status.success(),
        "GHC failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn int_value(value: &Value) -> i64 {
    match value {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if fields.len() == 1 => int_value(&fields[0]),
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn user_defined_append_matches_ghc() {
    require_extract();
    let expected = ghc_answer("customAppend");
    let result = EvalHarness::new().run_pure(SOURCE, "customAppend");
    assert_eq!(int_value(result.value()), expected);
}

#[test]
fn nul_string_matches_ghc() {
    require_extract();
    let expected = ghc_answer("nulString");
    let result = EvalHarness::new().run_pure(SOURCE, "nulString");
    assert_eq!(int_value(result.value()), expected);
}

#[test]
fn reference_evaluator_preserves_lazy_arguments() {
    require_extract();
    let expected = ghc_answer("lazyArgument");
    let compiled = EvalHarness::new().compile(SOURCE, "lazyArgument").unwrap();
    with_eval_stack(move || {
        let mut heap = VecHeap::new();
        let result = eval(
            &compiled.expr,
            &env_from_datacon_table(&compiled.table),
            &mut heap,
        )
        .and_then(|value| deep_force(value, &mut heap));
        assert_eq!(
            int_value(&result.expect("unused division must remain lazy")),
            expected
        );
    });
}

#[test]
fn jit_does_not_enter_unused_recursive_argument() {
    require_extract();
    let expected = ghc_answer("unusedLoop");
    let compiled = EvalHarness::new().compile(SOURCE, "unusedLoop").unwrap();
    with_eval_stack(move || {
        let mut machine =
            JitEffectMachine::compile(&compiled.expr, &compiled.table, 1024 * 1024).unwrap();
        let cancel = machine.cancel_handle();
        let (finished, completion) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if completion.recv_timeout(Duration::from_secs(1)).is_err() {
                cancel.cancel();
            }
        });
        let result = machine.run_pure();
        let _ = finished.send(());
        watchdog.join().unwrap();
        assert_eq!(
            int_value(&result.expect("unused recursion must remain lazy")),
            expected
        );
    });
}
