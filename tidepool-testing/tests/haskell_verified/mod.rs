use proptest::strategy::Strategy;
use proptest::test_runner::{Config, TestRunner};
use std::path::{Path, PathBuf};

pub mod cousins;
pub mod fmap;
pub mod text;

fn prelude_path() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

pub fn compile_run_pure(src: &str) -> serde_json::Value {
    let pp = prelude_path();
    let include = [pp.as_path()];

    // We wrap the src in a basic module template
    let full_src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T

result :: _
result = {}
"#,
        src
    );

    let val =
        tidepool_runtime::compile_and_run_pure(&full_src, "result", &include).unwrap_or_else(|e| {
            panic!(
                "compile_and_run_pure failed: {:?}\nSource:\n{}",
                e, full_src
            )
        });
    val.to_json()
}

pub fn compare_json(jit_value: &serde_json::Value, expected_value: &serde_json::Value, src: &str) {
    if jit_value != expected_value {
        panic!(
            "Mismatch!\nSource:\n{}\nExpected:\n{}\nActual:\n{}\n",
            src, expected_value, jit_value
        );
    }
}

pub fn run_template<S: Strategy<Value = (String, serde_json::Value)>>(cases: u32, strategy: S) {
    let mut runner = TestRunner::new(Config::with_cases(cases));
    let res = runner.run(&strategy, |(src, expected)| {
        let actual = compile_run_pure(&src);
        compare_json(&actual, &expected, &src);
        Ok(())
    });
    if let Err(e) = res {
        panic!("Proptest failed: {:?}", e);
    }
}

pub fn arb_int() -> impl Strategy<Value = i64> {
    -100i64..=100
}

pub fn arb_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"[a-zA-Z0-9 \.,!]{0,30}").unwrap()
}
