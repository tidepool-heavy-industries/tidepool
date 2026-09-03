//! Regression tests for foldl'/foldr over effect-returned lists.
//!
//! Bug: `foldl' (+) 0 xs` crashes with a null pointer when `xs` comes from
//! `forM`/`mapM` with effects. Pure lists and `map` over the same list work fine.
//! Forcing the spine first (via `length`) is a workaround.
//!
//! Bundled per `plans/test-time-cut.md` §3/§6 item 1: the 7 former standalone
//! `#[test]` fns (7 `tidepool-extract` spawns) are now 7 named top-level
//! bindings sharing ONE compiled module, compiled together in ONE spawn via
//! `EvalHarness::compile_many` (`compile_targets`'s N-in-one-spawn mode — an
//! extra target in the same GHC session costs ~2% more wall time, not another
//! session). Each is still run INDEPENDENTLY afterward (own `MockConsole`
//! instance, own dispatch) via `run_target_owned`, so per-check dispatch
//! interleaving/state is unaffected — this is the same isolation as 7
//! separate `#[test]` fns, one spawn instead of seven. Each check stays
//! individually named so a failure names exactly which one regressed.

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_testing::eval_harness::EvalHarness;

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

struct MockConsole {
    prints: Vec<String>,
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
                self.prints.push(s);
                cx.respond(())
            }
        }
    }
}

/// One `data Console`/`say` decl shared by all 7 checks below, plus one
/// top-level binding per former standalone test (name kept traceable to the
/// original `#[test]` fn in each comment).
const SRC: &str = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
     TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data Console a where
    Print :: Text -> Console ()

say :: Text -> Eff '[Console] ()
say = send . Print

-- test_foldl_over_effect_returned_list
resultFoldlOverEffectList :: Eff '[Console] Int
resultFoldlOverEffectList = do
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let total = foldl' (+) (0 :: Int) xs
  pure total

-- test_foldr_over_effect_returned_list
resultFoldrOverEffectList :: Eff '[Console] Int
resultFoldrOverEffectList = do
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let total = foldr (+) (0 :: Int) xs
  pure total

-- test_fold_with_snd_over_effect_tuples
resultFoldSndOverEffectTuples :: Eff '[Console] Int
resultFoldSndOverEffectTuples = do
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure ("x", i) })
  let total = foldl' (\acc x -> acc + snd x) (0 :: Int) xs
  pure total

-- test_map_over_effect_list_works
resultMapOverEffectListWorks :: Eff '[Console] [Int]
resultMapOverEffectListWorks = do
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let doubled = map (* 2) xs
  pure doubled

-- test_fold_after_spine_force_works
resultFoldAfterSpineForceWorks :: Eff '[Console] Int
resultFoldAfterSpineForceWorks = do
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  let _ = length xs
  let total = foldl' (+) (0 :: Int) xs
  pure total

-- test_fold_pure_list_works
resultFoldPureListWorks :: Eff '[Console] Int
resultFoldPureListWorks = do
  say "hello"
  let xs = [1, 2, 3 :: Int]
  let total = foldl' (+) (0 :: Int) xs
  pure total

-- test_show_effect_list_works
resultShowEffectListWorks :: Eff '[Console] [Int]
resultShowEffectListWorks = do
  xs <- forM [1, 2, 3 :: Int] (\i -> do { say ""; pure i })
  say (pack (show xs))
  pure xs
"#;

#[test]
fn works_effect_fold_family() {
    let harness = EvalHarness::new().with_stdlib();
    let artifacts = harness
        .compile_many(
            SRC,
            &[
                "resultFoldlOverEffectList",
                "resultFoldrOverEffectList",
                "resultFoldSndOverEffectTuples",
                "resultMapOverEffectListWorks",
                "resultFoldAfterSpineForceWorks",
                "resultFoldPureListWorks",
                "resultShowEffectListWorks",
            ],
        )
        .expect("compile_many failed");

    let mut failures: Vec<String> = Vec::new();
    let mut check = |name: &str, got: serde_json::Value, want: serde_json::Value| {
        if got != want {
            failures.push(format!("{name}: want {want}, got {got}"));
        }
    };

    let (out, _) = harness.run_target_owned(
        &artifacts,
        "resultFoldlOverEffectList",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_foldl_over_effect_returned_list",
        out.json(),
        serde_json::json!(6),
    );

    let (out, _) = harness.run_target_owned(
        &artifacts,
        "resultFoldrOverEffectList",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_foldr_over_effect_returned_list",
        out.json(),
        serde_json::json!(6),
    );

    let (out, _) = harness.run_target_owned(
        &artifacts,
        "resultFoldSndOverEffectTuples",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_fold_with_snd_over_effect_tuples",
        out.json(),
        serde_json::json!(6),
    );

    let (out, _) = harness.run_target_owned(
        &artifacts,
        "resultMapOverEffectListWorks",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_map_over_effect_list_works",
        out.json(),
        serde_json::json!([2, 4, 6]),
    );

    let (out, _) = harness.run_target_owned(
        &artifacts,
        "resultFoldAfterSpineForceWorks",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_fold_after_spine_force_works",
        out.json(),
        serde_json::json!(6),
    );

    let (out, _) = harness.run_target_owned(
        &artifacts,
        "resultFoldPureListWorks",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_fold_pure_list_works",
        out.json(),
        serde_json::json!(6),
    );

    let (out, console) = harness.run_target_owned(
        &artifacts,
        "resultShowEffectListWorks",
        frunk::hlist![MockConsole { prints: vec![] }],
    );
    check(
        "test_show_effect_list_works.value",
        out.json(),
        serde_json::json!([1, 2, 3]),
    );
    if !console.head.prints.iter().any(|s| s.contains('1')) {
        failures.push(
            "test_show_effect_list_works.console: expected a Print containing '1'".to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "{} check(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
