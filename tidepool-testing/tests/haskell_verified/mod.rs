use proptest::strategy::Strategy;
use proptest::test_runner::{Config, TestRunner};
use std::path::{Path, PathBuf};

pub mod cousins;
pub mod fmap;
pub mod list_ops;
pub mod map_set;
pub mod more_text_recursive;
pub mod numeric;
pub mod text;

fn prelude_path() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

pub fn compile_run_pure(src: &str) -> serde_json::Value {
    compile_run_pure_with_imports(src, "")
}

pub fn compile_run_pure_with_imports(src: &str, extra_imports: &str) -> serde_json::Value {
    let pp = prelude_path();
    let include = [pp.as_path()];

    let full_src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
{}

result :: _
result = {}
"#,
        extra_imports, src
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
    run_template_with_imports(cases, strategy, &[])
}

pub fn run_template_with_imports<S: Strategy<Value = (String, serde_json::Value)>>(
    cases: u32,
    strategy: S,
    extra_imports: &[&str],
) {
    let imports_str = extra_imports.join("\n");
    let mut runner = TestRunner::new(Config::with_cases(cases));
    let res = runner.run(&strategy, |(src, expected)| {
        let actual = compile_run_pure_with_imports(&src, &imports_str);
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

/// ASCII text with length in `[0, 30]`.
pub fn arb_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"[a-zA-Z0-9 \.,!]{0,30}").unwrap()
}
