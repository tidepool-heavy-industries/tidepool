//! Native-GHC controls for boxed-array initialization under finite floatToDigits.
use std::process::Command;
use tidepool_testing::eval_harness::EvalHarness;

const SOURCE: &str = include_str!("fixtures/floating/ArrayInitialization.hs");

fn compare_native(entry: &str) {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/floating/ArrayInitialization.hs");
    let output = Command::new("ghc")
        .arg(fixture)
        .args(["-e", &format!("putStr {entry}")])
        .output()
        .expect("native GHC requires repository Nix toolchain");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8(output.stdout).expect("UTF8 native result");
    let got = EvalHarness::new()
        .with_stdlib()
        .run_pure(SOURCE, entry)
        .json();
    assert_eq!(got, serde_json::Value::String(expected), "{entry}");
}

macro_rules! probe {
    ($name:ident, $entry:literal) => {
        #[test]
        fn $name() {
            compare_native($entry);
        }
    };
}
probe!(array_singleton, "singleResult");
probe!(array_list, "listResult");
probe!(array_associations, "associationResult");
probe!(array_initial_value, "initialResult");
probe!(array_write_preserves_lazy_untouched_cells, "writeResult");
probe!(array_loop_writes, "loopResult");
probe!(finite_float_to_digits, "digitsResult");
probe!(finite_prelude_show, "finiteShowResult");
probe!(array_write_defined_initializer, "writeDefinedResult");

probe!(array_unused_written_bottom, "unusedWrittenBottomResult");

fn compare_native_failure(entry: &str) {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/floating/ArrayInitialization.hs");
    let output = Command::new("ghc")
        .arg(fixture)
        .args(["-e", &format!("nativeFailure {entry}")])
        .output()
        .expect("native GHC");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"error-call");
    let got = EvalHarness::new().with_stdlib().run_pure(SOURCE, entry);
    assert!(
        matches!(
            got.err(),
            Some(tidepool_runtime::RuntimeError::Jit(
                tidepool_codegen::jit_machine::JitError::Yield(
                    tidepool_codegen::yield_type::YieldError::Runtime(
                        tidepool_codegen::host_fns::RuntimeError::UserErrorMsg(_)
                    )
                )
            ))
        ),
        "expected explicit user error, got {:?}",
        got.err()
    );
}

#[test]
fn array_selected_written_bottom() {
    compare_native_failure("selectedWrittenBottomResult");
}

#[test]
fn array_selected_bottom_initializer() {
    compare_native_failure("selectedInitializerResult");
}
