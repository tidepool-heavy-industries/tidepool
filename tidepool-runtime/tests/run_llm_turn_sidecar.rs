//! runLLMTurn (#R0 typed-yield pass): extract-side interception + asks.json
//! sidecar tests.
//!
//! Positive: a monomorphic `runLLMTurn @Verdict` site under both a branch
//! (an opaque NOINLINE'd `Bool`, so GHC can't const-fold away the untaken
//! arm) and a `mapM` loop (an opaque NOINLINE'd loop bound, so GHC can't
//! unroll the list literal into separate static call sites) — asserts the
//! asks.json sidecar lists one entry per SOURCE occurrence, and that the
//! runtime payload's "typedSite" value observed at each dispatch matches: the
//! taken branch's site fires once, the untaken branch's site never fires, and
//! the loop's single static site fires 3x with the SAME id every time.
//!
//! Negative: a polymorphic site fails extract (exact error text asserted).
//! A function-typed site is no longer categorically rejected (one-session
//! plan Phase 3e): a PURE function-typed answer (e.g. `Int -> Int`) now
//! compiles past `checkRunLLMTurnType`, and only an answer type that itself
//! mentions the `Eff` tycon (`M`/`Eff`) — e.g. `Int -> M Int` — is rejected,
//! since the row `M` expands to is still per-compile (stable-effects-core:
//! the effect GADTs themselves are now stable, but a compile's own row still
//! varies) — with the row-varies-per-compile error text.
//!
//! Run with the worktree extract binary, e.g.:
//!   TIDEPOOL_EXTRACT=<worktree>/haskell/dist-newstyle/.../tidepool-extract-bin \
//!   cargo test -p tidepool-runtime --test run_llm_turn_sidecar
use std::path::PathBuf;
use std::process::Command;

use tidepool_effect::dispatch::{DispatchEffect, EffectContext, Response};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_repr::Literal;
use tidepool_testing::eval_harness::{extract_env, prelude_path, EvalHarness};

/// `RunLLMTurn`'s union tag in `standard_decls()` — derived from its
/// POSITION in that list (Locked Decision: tags index the effect list
/// positionally), never hand-copied. `standard_decls()`'s own doc has the
/// story (self-iterating-harness WS-B split `runLLMTurn`/`runLLMTurnFork`/
/// `runLLMTurnFanout` out of `Ask` into their own effect/tag, same
/// `typedSite`/`fork`/`fan`/`prompts` payload shape, now riding a
/// `RunLLMTurnWith` Con instead of `AskWith`).
fn run_llm_turn_tag() -> u64 {
    tidepool_testing::effect_tags::tag_of(&tidepool_mcp::standard_decls(), "RunLLMTurn")
}

fn verdict_helpers() -> &'static str {
    "{-# NOINLINE loopCount #-}\n\
     loopCount :: Int\n\
     loopCount = 3\n\
     data Verdict = Approve | Reject deriving (Show)\n\
     data Outcome = Yes | No deriving (Show)\n"
}

fn verdict_code() -> &'static str {
    // The branch condition is itself an ask reply (`runLLMTurn @Bool
    // "gate"`), NOT a NOINLINE'd top-level CAF: GHC's simplifier turned out
    // to still constant-fold `case someNoinlineCaf of {...}` WITHIN THE SAME
    // module (NOINLINE only blocks CROSS-module inlining of the unfolding,
    // not this module's own local case-of-known-constructor pass — verified
    // empirically, `-ddump-simpl` showed the untaken arm gone even with
    // NOINLINE). An effect result is genuinely opaque to the simplifier
    // (dispatch happens outside GHC's view), so it can't be folded away.
    //
    // The two downstream branches instantiate runLLMTurn at DIFFERENT
    // answer types (Verdict vs Outcome): same-type branches
    // (`runLLMTurn @Verdict a` vs `runLLMTurn @Verdict b`) are one
    // opaque function applied to two different value arguments, which the
    // simplifier is free to float into ONE call site
    // (`runLLMTurn @Verdict (if c then a else b)`), merging what source
    // looks like two occurrences into one. Differing answer types block that
    // merge (Core has no value-level case over types) — this is also why the
    // spec's own example uses `@A`/`@B`, not `@A`/`@A`. Both branches are
    // voided to `M ()` (`>> pure ()`) since `if`/`then`/`else` needs one
    // unifiable type and Verdict/Outcome differ on purpose.
    "do\n\
     \x20 gate <- runLLMTurn @Bool \"gate\"\n\
     \x20 _ <- if gate then (runLLMTurn @Verdict \"branch-true\" >> pure ()) else (runLLMTurn @Outcome \"branch-false\" >> pure ())\n\
     \x20 _ <- mapM (\\i -> runLLMTurn @Verdict (T.pack (show (i :: Int)))) [1 .. loopCount]\n\
     \x20 pure (toJSON (42 :: Int))\n"
}

/// Records every dispatched Ask request's `"typedSite"` payload field.
/// Answers the FIRST dispatch (the `Bool` gate, which IS pattern-matched by
/// `if gate then ...`) with a real `True` Con looked up from the table;
/// every other dispatch's runLLMTurn bind is discarded (`_ <-`) so a
/// throwaway `Value` never gets forced.
struct SiteRecorder {
    sites: Vec<i64>,
}

impl DispatchEffect<()> for SiteRecorder {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        if tag != run_llm_turn_tag() {
            return Err(EffectError::UnhandledEffect { tag });
        }
        let Value::Con(_con_id, fields) = request else {
            return Err(EffectError::Handler(format!(
                "expected RunLLMTurnWith Con, got {request:?}"
            )));
        };
        let payload = fields
            .get(1)
            .ok_or_else(|| EffectError::Handler("RunLLMTurnWith missing payload field".into()))?;
        let json = tidepool_runtime::value_to_json(payload, cx.table(), 0);
        let site = json
            .get("typedSite")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                EffectError::Handler(format!("AskWith payload missing typedSite: {json}"))
            })?;
        let is_first = self.sites.is_empty();
        self.sites.push(site);
        if is_first {
            let true_id = cx
                .table()
                .get_by_name("True")
                .ok_or_else(|| EffectError::Handler("no True DataCon in table".into()))?;
            Ok(Response::Complete(Value::Con(true_id, vec![])))
        } else {
            Ok(Response::Complete(Value::Lit(Literal::LitInt(0))))
        }
    }
}

#[test]
fn runllmturn_site_ids_match_under_branch_and_loop() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(verdict_code()),
        "",
        verdict_helpers(),
        None,
        None,
    );

    let harness = EvalHarness::new()
        .with_stdlib()
        .with_effects_module()
        .with_extract_env();
    let (outcome, recorder) = harness.run_owned(&src, "result", SiteRecorder { sites: vec![] });
    outcome
        .into_result()
        .unwrap_or_else(|e| panic!("expected the eval to run to completion, got: {e}"));

    // 5 dispatches total: the gate + 1 taken branch arm + 3 loop iterations.
    assert_eq!(
        recorder.sites.len(),
        5,
        "expected 5 runLLMTurn dispatches (gate + branch + 3 loop), got {:?}",
        recorder.sites
    );
    let (gate_and_branch, loop_sites) = recorder.sites.split_at(2);
    let gate_site = gate_and_branch[0];
    let branch_site = gate_and_branch[1];

    // The loop's single static call site fires 3x with the SAME id every time.
    assert!(
        loop_sites.iter().all(|&s| s == loop_sites[0]),
        "loop iterations should all carry the SAME static site id, got {:?}",
        recorder.sites
    );
    // The gate, the branch, and the loop are three DISTINCT static occurrences.
    assert_ne!(
        gate_site, branch_site,
        "the gate and branch sites must differ"
    );
    assert_ne!(
        branch_site, loop_sites[0],
        "the branch site and the loop site must be distinct static occurrences"
    );
    assert_ne!(
        gate_site, loop_sites[0],
        "the gate and loop sites must differ"
    );

    // Sidecar: read asks.json from a kept-alive extract invocation over the
    // SAME source (bypassing `compile_haskell`'s auto-cleaned tempdir).
    let asks = compile_and_read_asks(&src, "result", &decls);
    assert_eq!(
        asks.as_array().map(|a| a.len()),
        Some(4),
        "expected 4 runLLMTurn sites (gate, branch-true, branch-false, loop), got {asks}"
    );
    let mut sidecar_sites = Vec::new();
    let mut sidecar_types = Vec::new();
    for entry in asks.as_array().unwrap() {
        let site = entry["site"]
            .as_i64()
            .unwrap_or_else(|| panic!("asks.json entry missing integer site: {entry}"));
        let ty = entry["type"]
            .as_str()
            .unwrap_or_else(|| panic!("asks.json entry missing string type: {entry}"));
        assert!(
            ty.contains("Bool") || ty.contains("Verdict") || ty.contains("Outcome"),
            "expected the rendered type to name Bool, Verdict, or Outcome, got {ty:?}"
        );
        sidecar_sites.push(site);
        sidecar_types.push(ty.to_string());
    }
    // 1 Bool site (the gate) + 2 Verdict sites (the taken branch + the loop)
    // + 1 Outcome site (the untaken branch — extract-time site assignment is
    // static, so a never-executed occurrence still gets its own entry).
    assert_eq!(
        sidecar_types.iter().filter(|t| t.contains("Bool")).count(),
        1,
        "expected 1 Bool site, got {sidecar_types:?}"
    );
    assert_eq!(
        sidecar_types
            .iter()
            .filter(|t| t.contains("Verdict"))
            .count(),
        2,
        "expected 2 Verdict sites, got {sidecar_types:?}"
    );
    assert_eq!(
        sidecar_types
            .iter()
            .filter(|t| t.contains("Outcome"))
            .count(),
        1,
        "expected 1 Outcome site, got {sidecar_types:?}"
    );
    sidecar_sites.sort_unstable();
    sidecar_sites.dedup();
    assert_eq!(
        sidecar_sites.len(),
        4,
        "asks.json site ids must be pairwise distinct, got {:?}",
        asks
    );

    // Every id actually observed at runtime resolves to a real sidecar entry.
    for &observed in &recorder.sites {
        assert!(
            sidecar_sites.contains(&observed),
            "dispatched site {observed} has no asks.json entry (sites: {sidecar_sites:?})"
        );
    }
}

/// Records every dispatched `RunLLMTurn` request's `"typedSite"` payload
/// field and answers EVERY dispatch with `Approve` (a `Verdict` DataCon) —
/// unlike `SiteRecorder` above, which is tailored to the Bool-gate/Verdict/
/// Outcome shape of `verdict_code` and answers subsequent dispatches with a
/// bare `LitInt`. A `Verdict`-only scenario needs a `Verdict`-shaped answer
/// on every dispatch, not just the first.
struct VerdictRecorder {
    sites: Vec<i64>,
}

impl DispatchEffect<()> for VerdictRecorder {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        if tag != run_llm_turn_tag() {
            return Err(EffectError::UnhandledEffect { tag });
        }
        let Value::Con(_con_id, fields) = request else {
            return Err(EffectError::Handler(format!(
                "expected RunLLMTurnWith Con, got {request:?}"
            )));
        };
        let payload = fields
            .get(1)
            .ok_or_else(|| EffectError::Handler("RunLLMTurnWith missing payload field".into()))?;
        let json = tidepool_runtime::value_to_json(payload, cx.table(), 0);
        let site = json
            .get("typedSite")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                EffectError::Handler(format!("AskWith payload missing typedSite: {json}"))
            })?;
        self.sites.push(site);
        let approve_id = cx
            .table()
            .get_by_name("Approve")
            .ok_or_else(|| EffectError::Handler("no Approve DataCon in table".into()))?;
        Ok(Response::Complete(Value::Con(approve_id, vec![])))
    }
}

/// siteid-plugin: `runLLMTurn` is now Member-polymorphic (`Member RunLLMTurn
/// effs => Text -> Eff effs a`, not fixed to the closed `M` stack — see
/// effect_defs.rs's RunLLMTurn helpers comment), so a REUSABLE helper can be
/// written generically over `effs` instead of needing the whole concrete
/// stack. Two DISTINCT top-level helpers (not two calls to the SAME helper —
/// that would legitimately CSE into one static site) each wrap one
/// `runLLMTurn @Verdict` call; asserts BOTH the runtime dispatch order and
/// the asks.json sidecar carry two DISTINCT site ids (the historical bug:
/// both collapsed to the placeholder `0`, this yielded `[0, 0]` instead of
/// `[0, 1]`).
#[test]
fn runllmturn_member_polymorphic_helper_gets_distinct_site_ids() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let helpers = "data Verdict = Approve | Reject deriving (Show)\n\
                   helper1 :: Member RunLLMTurn effs => Text -> Eff effs Verdict\n\
                   helper1 p = runLLMTurn @Verdict (p <> \"-one\")\n\
                   helper2 :: Member RunLLMTurn effs => Text -> Eff effs Verdict\n\
                   helper2 p = runLLMTurn @Verdict (p <> \"-two\")\n";
    let code = "do\n  \
                 a <- helper1 \"ask\"\n  \
                 b <- helper2 \"ask\"\n  \
                 pure (toJSON (show a <> show b))\n";
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", helpers, None, None);

    let harness = EvalHarness::new()
        .with_stdlib()
        .with_effects_module()
        .with_extract_env();
    let (outcome, recorder) = harness.run_owned(&src, "result", VerdictRecorder { sites: vec![] });
    outcome
        .into_result()
        .unwrap_or_else(|e| panic!("expected the eval to run to completion, got: {e}"));

    assert_eq!(
        recorder.sites.len(),
        2,
        "expected 2 dispatches (helper1 + helper2), got {:?}",
        recorder.sites
    );
    assert_ne!(
        recorder.sites[0], recorder.sites[1],
        "the two Member-polymorphic helper call sites must carry DISTINCT ids \
         (the historical bug collapsed both to the placeholder 0), got {:?}",
        recorder.sites
    );

    let asks = compile_and_read_asks(&src, "result", &decls);
    assert_eq!(
        asks.as_array().map(|a| a.len()),
        Some(2),
        "expected 2 runLLMTurn sites in the asks.json sidecar, got {asks}"
    );
}

#[test]
fn runllmturn_rejects_polymorphic_site() {
    // `@a` needs a real ScopedTypeVariables binder to be in scope (a bare `@a`
    // in `code` is just an out-of-scope type variable, a Haskell scoping
    // error unrelated to this feature) — a NOINLINE wrapper with its own
    // `forall a` gives `runLLMTurn` a genuinely free type variable at its
    // call site, exactly the shape `checkRunLLMTurnType` must reject.
    let helpers = "{-# NOINLINE polySite #-}\n\
                   polySite :: forall a. Text -> M a\n\
                   polySite prompt = runLLMTurn @a prompt\n";
    let err = try_compile_runllmturn("polySite \"poly\"", helpers)
        .expect_err("a polymorphic runLLMTurn site must fail extract");
    assert!(
        err.contains("polymorphic runLLMTurn site"),
        "expected the polymorphic-site error text, got:\n{err}"
    );
}

#[test]
fn runllmturn_accepts_pure_function_typed_site() {
    // One-session plan Phase 3e (TASK 1): `checkRunLLMTurnType` no longer
    // hard-rejects every function arrow — a PURE function-typed answer now
    // compiles past the gate (it may still fail LATER for unrelated
    // reasons; this only asserts the old blanket rejection is gone).
    let src_result = try_compile_runllmturn("runLLMTurn @(Int -> Int) \"fn\"", "");
    assert!(
        src_result.is_ok(),
        "a pure function-typed runLLMTurn site should compile past the \
         function-arrow gate now, got error: {:?}",
        src_result.err()
    );
}

#[test]
fn runllmturn_rejects_effectful_function_typed_site() {
    // The narrower TASK 1 rejection: an answer type that itself mentions the
    // effect monad (here `M Int` inside the arrow's result) is still a hard
    // compile-time error — the generated `M`/`Eff` is nominal PER FRAGMENT
    // and cannot unify across a suspend boundary.
    let err = try_compile_runllmturn("runLLMTurn @(Int -> M Int) \"fn\"", "")
        .expect_err("an effectful function-typed runLLMTurn site must fail extract");
    assert!(
        err.contains("effectful function answers not supported (the row varies per compile)"),
        "expected the new effectful-function-typed-site error text, got:\n{err}"
    );
    assert!(
        err.contains("the M inside cannot unify across turns/windows compiling a different row"),
        "expected the new error text's explanation clause, got:\n{err}"
    );
}

#[test]
fn runllmturn_accepts_monomorphic_data_site() {
    let src_result = try_compile_runllmturn(
        "runLLMTurn @Verdict \"ok\"",
        "data Verdict = Approve | Reject deriving (Show)",
    );
    assert!(
        src_result.is_ok(),
        "a monomorphic ADT site should compile cleanly, got error: {:?}",
        src_result.err()
    );
}

/// Compile `hole` (an expression using `runLLMTurn`, `>>= const (pure ())`'d
/// away so its polymorphic/never-forced result never needs a concrete
/// instantiation beyond the explicit `@T`) via the REAL eval pipeline,
/// returning the classified error message on failure.
fn try_compile_runllmturn(hole: &str, helpers: &str) -> Result<(), String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = format!("do\n  _ <- {hole}\n  pure (toJSON (0 :: Int))\n");
    let src = tidepool_mcp::template_haskell(&pre, &stack, &code, "", helpers, None, None);

    EvalHarness::new()
        .with_stdlib()
        .with_effects_module()
        .with_extract_env()
        .compile(&src, "result")
        .map(|_| ())
        .map_err(|e| tidepool_runtime::classify_compile(&e).message)
}

// ---------------------------------------------------------------------------
// forkMap (combinator-sites widen): the same extract-level recognition
// mechanism, generalized to a library combinator with a CALLER-chosen
// answer type. See Tidepool.Fork's module haddock for why this needed a
// new mechanism (forkFilter's fixed-Bool answer never did) and
// Tidepool.Translate's forkMap/forkCata case arm for the implementation.
// ---------------------------------------------------------------------------

/// Same as `try_compile_runllmturn`, but imports `Tidepool.Fork` so
/// `forkMap`/`forkCata` are in scope. `Tidepool.Fork` rides the `Fork`
/// effect (not `RunLLMTurn`); `standard_decls()` no longer declares `Fork`
/// (vestigial-subsystems review §4 — the ordinary session engine never
/// services it), so this widens the roster explicitly (tag 11, same as
/// before Fork moved out of `standard_decls()`) for its `Tidepool.Effects`
/// import to resolve.
///
/// Cannot use `.with_effects_module()`: that pulls the FIXED, shared
/// `Tidepool.Effects` module dir ([`tidepool_testing::eval_harness::
/// effects_include`]), generated from `standard_decls()` ALONE — which no
/// longer carries `Fork` either, so it would desync from `decls` here the
/// same way it would for `compile_and_read_asks` (see that fn's doc). Build
/// this call's own effects module dir from the widened `decls` instead.
fn try_compile_forkmap(hole: &str, helpers: &str) -> Result<(), String> {
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::fork_decl());
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = format!("do\n  _ <- {hole}\n  pure (toJSON (0 :: Int))\n");
    let src =
        tidepool_mcp::template_haskell(&pre, &stack, &code, "Tidepool.Fork", helpers, None, None);
    let effects_dirs = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .include_paths();

    EvalHarness::new()
        .with_stdlib()
        .with_includes(effects_dirs)
        .with_extract_env()
        .compile(&src, "result")
        .map(|_| ())
        .map_err(|e| tidepool_runtime::classify_compile(&e).message)
}

#[test]
fn forkmap_accepts_monomorphic_answer_type() {
    let src_result = try_compile_forkmap(
        "forkMap @Verdict (\\x -> T.pack (show (x :: Int))) [1, 2, 3 :: Int]",
        "data Verdict = Approve | Reject deriving (Show)",
    );
    assert!(
        src_result.is_ok(),
        "a monomorphic forkMap @Verdict call site should compile cleanly, got error: {:?}",
        src_result.err()
    );
}

/// The forkMap @Verdict call site's asks.json entry is INDISTINGUISHABLE in
/// shape from a bare `forkAll @Verdict` site — same "[Verdict]" type
/// string, same single-entry sidecar — because forkMapSited routes through
/// exactly one forkAllSited dispatch.
#[test]
fn forkmap_sidecar_entry_matches_bare_fanout_shape() {
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::fork_decl());
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = "do\n  ys <- forkMap @Verdict (\\x -> T.pack (show (x :: Int))) [1, 2, 3 :: Int]\n  pure (toJSON (length (ys :: [Verdict])))\n";
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        code,
        "Tidepool.Fork",
        "data Verdict = Approve | Reject deriving (Show)",
        None,
        None,
    );

    let asks = compile_and_read_asks(&src, "result", &decls);
    let entries = asks.as_array().expect("asks.json is an array");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly 1 forkMap site, got {asks}"
    );
    let ty = entries[0]["type"]
        .as_str()
        .unwrap_or_else(|| panic!("asks.json entry missing string type: {asks}"));
    assert!(
        ty.starts_with('[') && ty.ends_with(']') && ty.contains("Verdict"),
        "forkMap's sidecar type must match a bare fanout site's own \"[T]\" \
         convention, got {ty:?}"
    );
}

#[test]
fn forkmap_rejects_polymorphic_answer_type() {
    // Mirrors `runllmturn_rejects_polymorphic_site`: a NOINLINE wrapper
    // with its own `forall b` gives forkMap a genuinely free type variable
    // at its call site.
    let helpers = "{-# NOINLINE polyForkMap #-}\n\
                   polyForkMap :: forall b. (Int -> Text) -> [Int] -> M [b]\n\
                   polyForkMap f xs = forkMap @b f xs\n";
    let err = try_compile_forkmap("polyForkMap (\\x -> T.pack (show x)) [1, 2, 3]", helpers)
        .expect_err("a polymorphic forkMap site must fail extract");
    assert!(
        err.contains("polymorphic runLLMTurn site"),
        "expected the (shared) polymorphic-site error text, got:\n{err}"
    );
}

#[test]
fn forkmap_rejects_partial_application() {
    // `forkMap @Verdict f` alone (no list argument) never reaches the
    // full-application shape the extract arm rewrites — must fail at
    // extract naming the site, not fall through to forkMap's own dead
    // OPAQUE (runtime-error) stub.
    let helpers = "data Verdict = Approve | Reject deriving (Show)\n\
                   {-# NOINLINE useForkMap #-}\n\
                   useForkMap :: (Int -> Text) -> M ([Int] -> M [Verdict])\n\
                   useForkMap f = pure (forkMap @Verdict f)\n";
    let err = try_compile_forkmap("useForkMap (\\x -> T.pack (show x)) >> pure ()", helpers)
        .expect_err("a partially-applied forkMap site must fail extract");
    assert!(
        err.contains("forkMap") && err.contains("not fully applied"),
        "expected the forkMap partial-application error text, got:\n{err}"
    );
}

// ---------------------------------------------------------------------------
// fork (Tidepool.Fork's singleton sibling of forkAll): same extract-level
// recognition mechanism as forkAll, but head-swapped to forkSited (the Fork
// effect) so its sidecar type is recorded BARE (`T`, not `[T]`) — see
// Tidepool.Translate's isForkVar arm and Tidepool.Fork's `fork` haddock.
// ---------------------------------------------------------------------------

/// A `fork @Verdict "brief"` call site's asks.json entry records the BARE
/// answer type (`"Verdict"`), NOT the bracketed `"[Verdict]"` forkAll/forkMap
/// use — because `fork` head-swaps to `forkSited`, the single-answer
/// sibling of `forkAllSited` on the `Fork` effect.
#[test]
fn fork_sidecar_entry_records_bare_answer_type() {
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::fork_decl());
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = "do\n  y <- fork @Verdict \"brief\"\n  pure (toJSON (show (y :: Verdict)))\n";
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        code,
        "Tidepool.Fork",
        "data Verdict = Approve | Reject deriving (Show)",
        None,
        None,
    );

    let asks = compile_and_read_asks(&src, "result", &decls);
    let entries = asks.as_array().expect("asks.json is an array");
    assert_eq!(entries.len(), 1, "expected exactly 1 fork site, got {asks}");
    let ty = entries[0]["type"]
        .as_str()
        .unwrap_or_else(|| panic!("asks.json entry missing string type: {asks}"));
    assert_eq!(
        ty, "Verdict",
        "fork's sidecar type must be the bare answer type, not bracketed, got {ty:?}"
    );
}

/// Sibling check: a `forkAll @Verdict [...]` call site still records the
/// bracketed `"[Verdict]"` list type — confirms adding `fork`/`isForkVar`
/// left `forkAll`'s own recognition arm untouched.
#[test]
fn forkall_sidecar_entry_still_records_list_answer_type() {
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::fork_decl());
    let pre = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code =
        "do\n  ys <- forkAll @Verdict [\"a\", \"b\"]\n  pure (toJSON (length (ys :: [Verdict])))\n";
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        code,
        "Tidepool.Fork",
        "data Verdict = Approve | Reject deriving (Show)",
        None,
        None,
    );

    let asks = compile_and_read_asks(&src, "result", &decls);
    let entries = asks.as_array().expect("asks.json is an array");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly 1 forkAll site, got {asks}"
    );
    let ty = entries[0]["type"]
        .as_str()
        .unwrap_or_else(|| panic!("asks.json entry missing string type: {asks}"));
    assert!(
        ty.starts_with('[') && ty.ends_with(']') && ty.contains("Verdict"),
        "forkAll's sidecar type must still be the bracketed \"[T]\" convention, got {ty:?}"
    );
}

/// Invoke `tidepool-extract-bin` directly (mirroring `compile_haskell`'s own
/// `Command` construction) into a tempdir we keep alive, so `asks.json` (next
/// to `meta.cbor`, `writeWholeModuleClosed`'s sidecar) is still on disk to
/// read afterward — `compile_haskell`'s own tempdir is dropped before it
/// returns, and Rust-side sidecar consumption is a later segment, so there is
/// no production API yet that surfaces this path.
///
/// `decls` must be the SAME effect declarations `src` was templated against
/// (its `pre`/`stack` text names types like `Fork` that only exist in the
/// `Tidepool.Effects` module generated from a matching decl list) — the
/// fixed `effects_include()` dir is generated from `standard_decls()` alone
/// and does not carry `Fork` when a test adds `fork_decl()` on top of it.
fn compile_and_read_asks(
    source: &str,
    target: &str,
    decls: &[tidepool_mcp::EffectDecl],
) -> serde_json::Value {
    assert!(
        extract_env(),
        "tidepool-extract-bin must be resolvable (TIDEPOOL_EXTRACT or cabal build)"
    );
    let extract_bin =
        std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
    let temp_dir = tempfile::TempDir::new().expect("create tempdir");
    let input_path = temp_dir.path().join("Expr.hs");
    std::fs::write(&input_path, source).expect("write source");

    let effects_dir =
        tidepool_mcp::ensure_effects_module(decls).expect("write Tidepool.Effects module");
    let mut cmd = Command::new(&extract_bin);
    cmd.arg(&input_path);
    cmd.arg("--output-dir").arg(temp_dir.path());
    cmd.arg("--target").arg(target);
    cmd.arg("--include").arg(prelude_path());
    for path in effects_dir.include_paths() {
        cmd.arg("--include").arg(path);
    }
    let output = cmd.output().expect("spawn tidepool-extract-bin");
    assert!(
        output.status.success(),
        "extract failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let asks_path: PathBuf = temp_dir.path().join("asks.json");
    let asks_bytes =
        std::fs::read(&asks_path).unwrap_or_else(|e| panic!("read {}: {e}", asks_path.display()));
    serde_json::from_slice(&asks_bytes).unwrap_or_else(|e| {
        panic!(
            "parse asks.json: {e}\n{}",
            String::from_utf8_lossy(&asks_bytes)
        )
    })
}
