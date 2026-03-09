//! Regression test for SIGILL when `show (a + b)` is used where `a` and `b`
//! are Doubles extracted via lens access on a JSON Value.
//!
//! Minimal repro (pure Haskell, no effects needed):
//! ```haskell
//! let v = object ["x" .= True, "y" .= False]
//!     a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
//!     b = case v ^? key "y" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
//! in show (a + b)  -- SIGILL
//! ```
//!
//! `show a` and `show b` individually work. `a + b` works (returns Double).
//! Only `show (a + b)` triggers SIGILL. This is the root cause behind the
//! `??` operator crash (`h_conf` computes `(b1 + b2 + b3) / 3.0` from lens
//! extractions, then the result gets `show`n in the `ask` fallback path).

mod common;

use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_runtime::{compile_and_run_pure, compile_haskell, value_to_json};

fn prelude_path() -> std::path::PathBuf {
    common::prelude_path()
}

fn run(src: &str) -> serde_json::Value {
    let pp = prelude_path();
    let src = src.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            compile_and_run_pure(&src, "result", &include)
                .expect("compile_and_run_pure failed")
                .to_json()
        })
        .unwrap()
        .join()
        .unwrap()
}

fn dump_core(src: &str) -> String {
    let pp = prelude_path();
    let include = [pp.as_path()];
    let (expr, _table, _warnings) =
        compile_haskell(src, "result", &include).expect("compile_haskell failed");
    tidepool_repr::pretty::pretty_print(&expr)
}

// === Controls: should all pass ===

#[test]
fn test_show_double_literal() {
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result = show (3.0 :: Double)
"#);
    assert_eq!(json, serde_json::json!("3.0"));
}

#[test]
fn test_show_double_addition_literals() {
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result = show (1.0 + 2.0 :: Double)
"#);
    assert_eq!(json, serde_json::json!("3.0"));
}

#[test]
fn test_show_double_from_case_on_bool() {
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let a = case True of { True -> 1.0; False -> 0.0 :: Double }
      b = case True of { True -> 2.0; False -> 0.0 :: Double }
  in show (a + b)
"#);
    assert_eq!(json, serde_json::json!("3.0"));
}

#[test]
fn test_lens_extract_individual_show() {
    // show on individual lens-extracted Doubles works
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let v = object ["x" .= True, "y" .= False]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case v ^? key "y" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show a <> " " <> show b
"#);
    assert_eq!(json, serde_json::json!("1.0 0.0"));
}

#[test]
fn test_lens_extract_addition_no_show() {
    // a + b without show works
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Double
result =
  let v = object ["x" .= True, "y" .= False]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case v ^? key "y" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
  in a + b
"#);
    // serde_json normalizes 1.0 to 1 (no fractional part)
    assert!(json.is_number());
    assert_eq!(json.as_f64().unwrap(), 1.0);
}

// === Bug repros: thunk-in-box fix ===

#[test]
fn test_show_sum_of_lens_extracted_doubles() {
    // SIGILL: show (a + b) where a, b from lens on Value
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let v = object ["x" .= True, "y" .= False]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case v ^? key "y" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show (a + b)
"#);
    assert_eq!(json, serde_json::json!("1.0"));
}

#[test]
fn test_h_conf_pattern() {
    // The exact h_conf pattern from the ?? operator
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let v = object ["_understood" .= True, "_confident" .= False, "_unambiguous" .= True]
      b k = case v ^? key k . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      c = (b "_understood" + b "_confident" + b "_unambiguous") / 3.0
  in show c
"#);
    let s = json.as_str().unwrap();
    let n: f64 = s.parse().unwrap();
    assert!((n - 0.6666).abs() < 0.01);
}

#[test]
fn test_show_double_on_lens_sum_via_show_double() {
    // showDouble returns String, so wrap with pack for Text result
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let v = object ["x" .= True, "y" .= True]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case v ^? key "y" . _Bool of { Just True -> 2.0; _ -> 0.0 :: Double }
  in pack (showDouble (a + b))
"#);
    assert_eq!(json, serde_json::json!("3.0"));
}

// === Minimal isolation test ===

#[test]
fn test_show_double_from_maybe_case() {
    // Reproduce the failing pattern WITHOUT lens: case on Maybe with Double
    // This tests if the issue is lens-specific or general Maybe-case-to-Double
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let a = case Just True of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case Nothing of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show (a + b)
"#);
    assert_eq!(json, serde_json::json!("1.0"));
}

#[test]
fn test_show_double_opaque_maybe() {
    // Use a function to prevent constant folding
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case mkMaybe False of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show (a + b)
"#);
    assert_eq!(json, serde_json::json!("1.0"));
}

#[test]
fn test_show_double_noinline_single() {
    // Even simpler: SINGLE opaque Maybe case, no addition
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show a
"#);
    assert_eq!(json, serde_json::json!("1.0"));
}

#[test]
fn test_show_double_noinline_add_no_show() {
    // a + b without show — should pass
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Double
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case mkMaybe False of { Just True -> 1.0; _ -> 0.0 :: Double }
  in a + b
"#);
    assert!(json.is_number());
    assert_eq!(json.as_f64().unwrap(), 1.0);
}

#[test]
fn test_show_int_noinline() {
    // Same pattern but with Int instead of Double — is it Double-specific?
    let json = run(r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1; _ -> 0 :: Int }
      b = case mkMaybe False of { Just True -> 1; _ -> 0 :: Int }
  in show (a + b)
"#);
    assert_eq!(json, serde_json::json!("1"));
}

// === Diagnostic tests: dump GHC Core for comparison ===

#[test]
fn test_dump_core_noinline_double_fail() {
    // Minimal SIGILL: show (a + b :: Double) with NOINLINE
    let core = dump_core(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case mkMaybe False of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show (a + b)
"#,
    );
    eprintln!("=== FAILING: show (a+b :: Double) NOINLINE ===\n{}\n", core);
}

#[test]
fn test_dump_core_noinline_int_pass() {
    // Same pattern with Int (PASSES)
    let core = dump_core(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1; _ -> 0 :: Int }
      b = case mkMaybe False of { Just True -> 1; _ -> 0 :: Int }
  in show (a + b)
"#,
    );
    eprintln!("=== PASSING: show (a+b :: Int) NOINLINE ===\n{}\n", core);
}

#[test]
fn test_dump_core_passing_case() {
    // show (a + b) where a,b from case on Bool literal — this PASSES
    let core = dump_core(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let a = case True of { True -> 1.0; False -> 0.0 :: Double }
      b = case True of { True -> 2.0; False -> 0.0 :: Double }
  in show (a + b)
"#,
    );
    eprintln!("=== PASSING CORE (case on Bool literal) ===\n{}\n", core);
}

#[test]
fn test_dump_core_failing_case() {
    // show (a + b) where a,b from lens — this SIGILL's at runtime
    let core = dump_core(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let v = object ["x" .= True, "y" .= False]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case v ^? key "y" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show (a + b)
"#,
    );
    eprintln!("=== FAILING CORE (lens-extracted Doubles) ===\n{}\n", core);
}

#[test]
fn test_dump_core_show_a_only() {
    // show a (individual, no addition) where a from lens — this PASSES
    let core = dump_core(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result =
  let v = object ["x" .= True]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show a
"#,
    );
    eprintln!("=== PASSING CORE (show a, single lens) ===\n{}\n", core);
}

#[test]
fn test_dump_core_addition_no_show() {
    // a + b without show where a,b from lens — this PASSES
    let core = dump_core(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Double
result =
  let v = object ["x" .= True, "y" .= False]
      a = case v ^? key "x" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case v ^? key "y" . _Bool of { Just True -> 1.0; _ -> 0.0 :: Double }
  in a + b
"#,
    );
    eprintln!("=== PASSING CORE (a + b, no show) ===\n{}\n", core);
}

// === Interpreter test: confirm JIT-specific bug ===

fn run_interp(src: &str) -> Result<serde_json::Value, tidepool_eval::EvalError> {
    let pp = prelude_path();
    let include = [pp.as_path()];
    let (expr, table, _warnings) =
        compile_haskell(src, "result", &include).expect("compile_haskell failed");
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let val = eval(&expr, &env, &mut heap)?;
    let forced = deep_force(val, &mut heap)?;
    Ok(value_to_json(&forced, &table, 0))
}

#[test]
fn test_interpreter_show_double_noinline() {
    // Same bug-triggering pattern but via the tree-walking interpreter.
    // If this passes, the bug is JIT-specific.
    let pp = prelude_path();
    let include = [pp.as_path()];
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case mkMaybe False of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show (a + b)
"#;
    let (expr, table, _warnings) =
        compile_haskell(src, "result", &include).expect("compile_haskell failed");
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let result = eval(&expr, &env, &mut heap);
    match &result {
        Ok(val) => {
            let forced = deep_force(val.clone(), &mut heap).expect("deep_force failed");
            let json = value_to_json(&forced, &table, 0);
            assert_eq!(json, serde_json::json!("1.0"));
        }
        Err(e) => {
            // Dump the core for debugging
            let core = tidepool_repr::pretty::pretty_print(&expr);
            eprintln!("=== INTERPRETER ERROR: {} ===", e);
            eprintln!("=== CORE ===\n{}", core);
            panic!("interpreter failed: {}", e);
        }
    }
}

#[test]
fn test_interpreter_show_double_simple() {
    let json = run_interp(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
result :: Text
result = show (1.0 + 2.0 :: Double)
"#,
    )
    .unwrap();
    assert_eq!(json, serde_json::json!("3.0"));
}

#[test]
fn test_interpreter_addition_no_show_noinline() {
    let json = run_interp(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Double
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
      b = case mkMaybe False of { Just True -> 1.0; _ -> 0.0 :: Double }
  in a + b
"#,
    )
    .unwrap();
    assert!(json.is_number());
    assert_eq!(json.as_f64().unwrap(), 1.0);
}

#[test]
fn test_interpreter_show_single_noinline() {
    // show a (single, no addition) where a from NOINLINE
    let json = run_interp(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1.0; _ -> 0.0 :: Double }
  in show a
"#,
    )
    .unwrap();
    assert_eq!(json, serde_json::json!("1.0"));
}

#[test]
fn test_interpreter_show_int_noinline() {
    // Same pattern but Int instead of Double
    let json = run_interp(
        r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
{-# NOINLINE mkMaybe #-}
mkMaybe :: Bool -> Maybe Bool
mkMaybe True = Just True
mkMaybe False = Nothing

result :: Text
result =
  let a = case mkMaybe True of { Just True -> 1; _ -> 0 :: Int }
      b = case mkMaybe False of { Just True -> 1; _ -> 0 :: Int }
  in show (a + b)
"#,
    )
    .unwrap();
    assert_eq!(json, serde_json::json!("1"));
}
