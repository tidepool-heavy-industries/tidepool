//! Regression test for SIGILL when a string derived from effect A's list result
//! is captured across effect B's dispatch in a branching continuation.
//!
//! Bug: In the MCP server, `sgRuleFind` returns `[Match]`, then `pick/yn ?? prompt`
//! where `prompt` contains `show (length matches)` crashes with SIGILL. The `??`
//! operator compiles to code that fires `llmJson` then conditionally fires `ask`,
//! both using `prompt`. The continuation for `llmJson` captures `prompt` for the
//! `ask` fallback branch. After `llmJson` returns and the continuation resumes,
//! the captured `prompt` pointer is stale (likely GC-forwarded), causing SIGILL
//! on case-match in the continuation code.
//!
//! Reproduces: SIGILL (exhausted case branch) in JIT
//! Root cause hypothesis: `apply_cont_heap` reads k2 before calling k1, then uses
//! stale k2 after GC may have run inside k1's execution.

mod common;

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run_with_nursery_size;

fn prelude_path() -> std::path::PathBuf {
    common::prelude_path()
}

// --- Effect A: DataSource (returns a list of strings) ---

#[derive(FromCore)]
enum DataSourceReq {
    #[core(name = "GetItems")]
    GetItems,
}

struct MockDataSource;

impl EffectHandler for MockDataSource {
    type Request = DataSourceReq;
    fn handle(&mut self, req: DataSourceReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            DataSourceReq::GetItems => {
                // Return a list of 50 strings to fill nursery
                let items: Vec<String> = (0..50)
                    .map(|i| {
                        format!(
                            "item_{:04}_padding_to_make_this_longer_{}",
                            i,
                            "x".repeat(50)
                        )
                    })
                    .collect();
                cx.respond(items)
            }
        }
    }
}

// --- Effect B: Classifier (takes text, returns text) ---

#[derive(FromCore)]
enum ClassifierReq {
    #[core(name = "Classify")]
    Classify(String),
}

struct MockClassifier;

impl EffectHandler for MockClassifier {
    type Request = ClassifierReq;
    fn handle(&mut self, req: ClassifierReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ClassifierReq::Classify(_prompt) => cx.respond("ok".to_string()),
        }
    }
}

// --- Effect C: Console (for say) ---

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

struct MockConsole;

impl EffectHandler for MockConsole {
    type Request = ConsoleReq;
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Print(_s) => cx.respond(()),
        }
    }
}

/// Run Haskell with three effects: DataSource, Classifier, Console.
/// Uses a small nursery to increase GC pressure.
fn run_three_effects(body: &str, nursery_size: usize) -> tidepool_runtime::EvalResult {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
             TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data DataSource a where
    GetItems :: DataSource [Text]

data Classifier a where
    Classify :: Text -> Classifier Text

data Console a where
    Print :: Text -> Console ()

getItems :: Eff '[DataSource, Classifier, Console] [Text]
getItems = send GetItems

classify :: Text -> Eff '[DataSource, Classifier, Console] Text
classify = send . Classify

say :: Text -> Eff '[DataSource, Classifier, Console] ()
say = send . Print

result :: Eff '[DataSource, Classifier, Console] _
result = do
{body}
"#
    );
    let pp = prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![MockDataSource, MockClassifier, MockConsole];
            compile_and_run_with_nursery_size(
                &src,
                "result",
                &include,
                &mut handlers,
                &(),
                nursery_size,
            )
            .expect("compile_and_run failed")
        })
        .unwrap()
        .join()
        .unwrap()
}

// === Reproducing the MCP SIGILL bug ===

/// Core repro: get list from effect A, derive string from its length,
/// pass that string to effect B where it's captured in a branching continuation.
///
/// This mirrors the `??` operator pattern:
///   r <- classify prompt          -- effect B fires, continuation captures prompt
///   if r == "ok" then pure prompt -- uses prompt again after resume
///                 else pure (prompt <> " - " <> r)
#[test]
fn test_derived_prompt_across_effect_boundary() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items. Classify this."
  say prompt
  r <- classify prompt
  if r == "ok"
    then pure prompt
    else pure (prompt <> " - " <> r)
"#,
        1 << 26, // 64 MiB (default)
    );
    let json = result.to_json();
    assert_eq!(json, serde_json::json!("Found 50 items. Classify this."));
}

/// Same test but with smaller nursery to force GC.
#[test]
fn test_derived_prompt_across_effect_small_nursery() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items. Classify this."
  say prompt
  r <- classify prompt
  if r == "ok"
    then pure prompt
    else pure (prompt <> " - " <> r)
"#,
        1 << 20, // 1 MiB nursery — much more GC pressure
    );
    let json = result.to_json();
    assert_eq!(json, serde_json::json!("Found 50 items. Classify this."));
}

/// Variant: use the list contents (not just length) in the prompt string.
#[test]
fn test_list_contents_in_prompt_across_effect() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let first3 = take 3 items
  let prompt = "Items: " <> show first3 <> " (total: " <> show (length items) <> ")"
  say prompt
  r <- classify prompt
  if r == "ok"
    then pure prompt
    else pure (prompt <> " => " <> r)
"#,
        1 << 20,
    );
    let json = result.to_json();
    // Just verify it doesn't crash and returns a string
    assert!(json.is_string());
    let s = json.as_str().unwrap();
    assert!(s.contains("total: 50"));
}

/// Control: literal prompt (no derivation from effect result) — should always work.
#[test]
fn test_literal_prompt_across_effect_works() {
    let result = run_three_effects(
        r#"
  items <- getItems
  say ("Got " <> show (length items))
  r <- classify "Is this an API?"
  pure r
"#,
        1 << 20,
    );
    assert_eq!(result.to_json(), serde_json::json!("ok"));
}

/// Control: derive string and use it ONLY for say (no second effect) — should always work.
#[test]
fn test_derived_prompt_say_only_works() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items."
  say prompt
  pure prompt
"#,
        1 << 20,
    );
    assert_eq!(result.to_json(), serde_json::json!("Found 50 items."));
}

/// Variant: multiple classify calls with derived prompts (deeper continuation nesting).
#[test]
fn test_multiple_derived_prompts() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let n = length items
  r1 <- classify ("batch 1: " <> show n)
  r2 <- classify ("batch 2: " <> show n <> " prev=" <> r1)
  pure (r1 <> "," <> r2)
"#,
        1 << 20,
    );
    assert_eq!(result.to_json(), serde_json::json!("ok,ok"));
}

/// The forM + classify pattern (closest to the MCP sgRuleFind + ?? loop).
#[test]
fn test_for_m_classify_with_derived_prompt() {
    let result = run_three_effects(
        r#"
  items <- getItems
  results <- forM (take 3 items) (\item -> do
    let prompt = "Classify: " <> item <> " (of " <> show (length items) <> ")"
    classify prompt)
  pure results
"#,
        1 << 20,
    );
    let json = result.to_json();
    assert_eq!(json, serde_json::json!(["ok", "ok", "ok"]));
}

/// Mimics the `??` operator: effect B fires, then a conditional either returns
/// directly or fires effect C (a different effect). The prompt is captured for
/// both branches. This is the closest match to the MCP crash pattern where
/// `llmJson` fires and the `ask` fallback is in the continuation.
#[test]
fn test_classify_then_conditional_say_with_derived_prompt() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items. Classify this."
  -- Effect B fires (classify), continuation captures prompt for fallback
  r <- classify prompt
  -- Conditional: either return or fire effect C (say) using prompt
  v <- if r == "ok"
         then pure prompt
         else do { say (prompt <> " FALLBACK: " <> r); pure r }
  pure v
"#,
        1 << 20,
    );
    let json = result.to_json();
    assert_eq!(json, serde_json::json!("Found 50 items. Classify this."));
}

/// Same as above but trigger the fallback branch (effect C fires after effect B).
#[test]
fn test_classify_then_fallback_say_with_derived_prompt() {
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
             TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data DataSource a where
    GetItems :: DataSource [Text]

data Classifier a where
    Classify :: Text -> Classifier Text

data Console a where
    Print :: Text -> Console ()

getItems :: Eff '[DataSource, Classifier, Console] [Text]
getItems = send GetItems

classify :: Text -> Eff '[DataSource, Classifier, Console] Text
classify = send . Classify

say :: Text -> Eff '[DataSource, Classifier, Console] ()
say = send . Print

result :: Eff '[DataSource, Classifier, Console] _
result = do
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items."
  r <- classify prompt
  -- Unconditionally fire a SECOND effect using prompt
  say (prompt <> " classified as: " <> r)
  -- Then use prompt AGAIN in a third context
  pure (prompt <> " => " <> r)
"#
    .to_string();
    let pp = prelude_path();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            // Classifier returns "fail" to trigger fallback path
            let mut handlers = frunk::hlist![
                MockDataSource,
                MockClassifierCustom("fail".to_string()),
                MockConsole
            ];
            compile_and_run_with_nursery_size(&src, "result", &include, &mut handlers, &(), 1 << 20)
                .expect("compile_and_run failed")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    let s = json.as_str().unwrap();
    assert!(s.contains("Found 50 items."));
    assert!(s.contains("fail"));
}

struct MockClassifierCustom(String);

impl EffectHandler for MockClassifierCustom {
    type Request = ClassifierReq;
    fn handle(&mut self, req: ClassifierReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ClassifierReq::Classify(_) => cx.respond(self.0.clone()),
        }
    }
}

/// Extreme nursery pressure: return large data, tiny nursery.
/// This attempts to force GC during the continuation application.
#[test]
fn test_extreme_nursery_pressure() {
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
             TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data DataSource a where
    GetItems :: DataSource [Text]

data Classifier a where
    Classify :: Text -> Classifier Text

data Console a where
    Print :: Text -> Console ()

getItems :: Eff '[DataSource, Classifier, Console] [Text]
getItems = send GetItems

classify :: Text -> Eff '[DataSource, Classifier, Console] Text
classify = send . Classify

say :: Text -> Eff '[DataSource, Classifier, Console] ()
say = send . Print

result :: Eff '[DataSource, Classifier, Console] _
result = do
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items."
  say prompt
  -- Fire classify, then classify AGAIN with a different prompt,
  -- then use both results — deep continuation nesting
  r1 <- classify prompt
  r2 <- classify (prompt <> " r1=" <> r1)
  -- Use items, prompt, r1, r2 all in the result (all captured across effects)
  let summary = prompt <> " | " <> r1 <> " | " <> r2 <> " | head=" <> head items
  pure summary
"#
    .to_string();
    let pp = prelude_path();
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![MockDataSource, MockClassifier, MockConsole];
            compile_and_run_with_nursery_size(
                &src,
                "result",
                &include,
                &mut handlers,
                &(),
                1 << 18, // 256 KiB — very tight
            )
            .expect("compile_and_run failed")
        })
        .unwrap()
        .join()
        .unwrap();
    let json = result.to_json();
    let s = json.as_str().unwrap();
    assert!(s.contains("Found 50 items."));
    assert!(s.contains("head=item_0000"));
}

/// The real pattern: effect B returns a Value (not String), and the continuation
/// parses it then conditionally fires effect C. This matches `llmJson` returning
/// JSON that gets parsed, then `ask` firing if confidence is low.
#[test]
fn test_classify_value_then_conditional_branch() {
    let result = run_three_effects(
        r#"
  items <- getItems
  let count = length items
  let prompt = "Found " <> show count <> " items."
  say prompt
  -- classify returns "ok" — simulate llmJson returning JSON
  r <- classify prompt
  -- Parse the result and conditionally branch (like ?? confidence check)
  let parsed = r
  v <- if parsed == "ok"
         then pure prompt              -- fast path: reuse prompt
         else do
           say ("Low confidence on: " <> prompt)  -- fallback: fire Console with prompt
           pure (prompt <> " [escalated]")
  pure v
"#,
        1 << 20,
    );
    assert_eq!(result.to_json(), serde_json::json!("Found 50 items."));
}
