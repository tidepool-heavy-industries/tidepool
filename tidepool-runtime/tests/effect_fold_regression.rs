//! Regression tests for foldl'/foldr over effect-returned lists.
//!
//! Bug: `foldl' (+) 0 xs` crashes with a null pointer when `xs` comes from
//! `forM`/`mapM` with effects. Pure lists and `map` over the same list work fine.
//! Forcing the spine first (via `length`) is a workaround.

use std::sync::{Arc, Mutex};

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_testing::eval_harness::EvalHarness;

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

struct MockConsole {
    prints: Arc<Mutex<Vec<String>>>,
}

impl EffectHandler for MockConsole {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                self.prints.lock().unwrap().push(s);
                cx.respond(())
            }
        }
    }
}

/// Run effectful Haskell with a Console handler.
fn run_with_console(body: &str) -> (tidepool_runtime::EvalResult, Vec<String>) {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
             TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data Console a where
    Print :: Text -> Console ()

say :: Text -> Eff '[Console] ()
say = send . Print

result :: Eff '[Console] _
result = do
{body}
"#
    );
    let prints = Arc::new(Mutex::new(Vec::new()));
    let handlers = frunk::hlist![MockConsole {
        prints: prints.clone()
    }];
    let result = EvalHarness::new()
        .with_stdlib()
        .run(&src, "result", handlers)
        .expect("compile_and_run failed");
    let prints = Arc::try_unwrap(prints).unwrap().into_inner().unwrap();
    (result, prints)
}

// === Bug repro tests (expected to FAIL until fix) ===

#[test]
fn test_foldl_over_effect_returned_list() {
    let (result, _console) = run_with_console(
        r#"
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let total = foldl' (+) (0 :: Int) xs
  pure total
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!(6));
}

#[test]
fn test_foldr_over_effect_returned_list() {
    let (result, _console) = run_with_console(
        r#"
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let total = foldr (+) (0 :: Int) xs
  pure total
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!(6));
}

#[test]
fn test_fold_with_snd_over_effect_tuples() {
    let (result, _console) = run_with_console(
        r#"
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure ("x", i) })
  let total = foldl' (\acc x -> acc + snd x) (0 :: Int) xs
  pure total
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!(6));
}

// === Control tests (expected to PASS) ===

#[test]
fn test_map_over_effect_list_works() {
    let (result, _console) = run_with_console(
        r#"
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let doubled = map (* 2) xs
  pure doubled
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!([2, 4, 6]));
}

#[test]
fn test_fold_after_spine_force_works() {
    let (result, _console) = run_with_console(
        r#"
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let _ = length xs
  let total = foldl' (+) (0 :: Int) xs
  pure total
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!(6));
}

#[test]
fn test_fold_pure_list_works() {
    let (result, _console) = run_with_console(
        r#"
  say "hello"
  let xs = [1, 2, 3 :: Int]
  let total = foldl' (+) (0 :: Int) xs
  pure total
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!(6));
}

#[test]
fn test_show_effect_list_works() {
    let (result, console) = run_with_console(
        r#"
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  say (show xs)
  pure xs
"#,
    );
    assert_eq!(result.to_json(), serde_json::json!([1, 2, 3]));
    assert!(console.iter().any(|s| s.contains("1")));
}
