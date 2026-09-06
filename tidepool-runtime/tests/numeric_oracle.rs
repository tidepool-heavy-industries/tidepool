//! Compare the same pure source through native GHC and the extractor/JIT.
//! NaN payload preservation is not asserted. The native oracle determines
//! platform Show spelling; signed zeros remain visible in the string result.
use std::process::Command;
use tidepool_testing::eval_harness::EvalHarness;

const SOURCE: &str = include_str!("fixtures/floating/NumericContract.hs");

fn compare_native(entry: &str) {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/floating/NumericContract.hs");
    let output = Command::new("ghc")
        .arg(&fixture)
        .args(["-e", &format!("putStr {entry}")])
        .output()
        .expect("native GHC oracle requires the repository Nix toolchain");
    assert!(
        output.status.success(),
        "native oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8(output.stdout).expect("GHC oracle emitted non-UTF8");
    let got = EvalHarness::new()
        .with_stdlib()
        .run_pure(SOURCE, entry)
        .json();
    assert_eq!(
        got,
        serde_json::Value::String(expected),
        "native GHC / Tidepool disagreement for {entry}"
    );
}

#[test]
fn numeric_native_double_classification_and_show() {
    compare_native("doubleResult");
}

#[test]
fn numeric_native_float_classification_and_show() {
    compare_native("floatResult");
}

#[test]
fn numeric_native_arithmetic_conversion_rounding() {
    compare_native("arithmeticResult");
}
