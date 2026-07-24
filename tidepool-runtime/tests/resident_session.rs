//! Segment 20 VERIFY — the resident-session engine end-to-end (GHC-heavy tier).
//!
//! Drives the REAL production path: GHC → Core extract → JIT `compile_session`
//! → `add_function` fragment → `run_fragment_suspendable` / `resume_suspended`,
//! all through [`tidepool_runtime::session::ResidentSession`]. Proves the DONE
//! criterion: a resident session survives run → suspend at an `ask` → (the eval
//! thread exits, nothing parked) → resume with a value → turn completes →
//! machine back in slot → next turn sees prior state.
//!
//! Requires `TIDEPOOL_EXTRACT` (the GHC→Core extractor) on the environment,
//! same as the other GHC-heavy `tidepool-runtime` tests (see
//! `session_decl_accum.rs`). `--ignore-default-filter` is needed to run it.
//!
//! # What "prior state visible next turn" means here
//!
//! Segment 20 keeps the MACHINE resident (retained heap, same JIT module) and
//! the HANDLER STACK resident. Cross-turn state is proven through the
//! effect-plane accumulator (the resident `MockKv`'s `HashMap` survives across
//! turns) — the idiomatic tidepool "KV-as-IORef" shape. The value-plane
//! tenure-across-suspend (a session `RootSlot` referenced by a later fragment's
//! `ExternalEnv`) needs the stowed-continuation GC root, which is segment 40;
//! this suite deliberately does not exercise it (see the segment-20 SPEC's L7
//! anti-pattern).

use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::Literal;
use tidepool_runtime::session::{ResidentError, ResidentOutcome, ResidentSession};
use tidepool_runtime::{value_to_json, DEFAULT_NURSERY_SIZE};

use tidepool_testing::eval_harness::{self, mock, EvalHarness};

/// A bridged integer answer value (the `ask` reply the resident turn resumes on).
fn int(n: i64) -> Value {
    Value::Lit(Literal::LitInt(n))
}

// ---------------------------------------------------------------------------
// Test sink + handler-stack adapter
// ---------------------------------------------------------------------------

/// A trivial `OutputSink` — the resident session drains/snapshots it around
/// turns. `Arc`-backed so it is `Clone + Send + Sync` (the resident session
/// runs each turn on a fresh eval thread and clones the sink onto it).
#[derive(Clone, Default)]
struct TestSink {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl tidepool_runtime::session::OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// Adapt a `DispatchEffect<()>` handler stack (like [`mock::min_stack`]) to the
/// `DispatchEffect<TestSink>` the resident session drives — the mock stack
/// ignores its user context, so we hand each dispatch a fresh `()`-context over
/// the same table. Keeps `ResidentSession`'s production `U == O == sink` shape
/// while reusing the ready-made mock stack.
struct AsSink<H>(H);

impl<H: DispatchEffect<()>> DispatchEffect<TestSink> for AsSink<H> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
        let unit_cx = EffectContext::with_user(cx.table(), &());
        self.0.dispatch(tag, request, &unit_cx)
    }
}

// `Ask` is the last effect in the base stack (index 9).
const ASK_TAG: u64 = (mock::EFFECT_NAMES.len() as u64) - 1;

/// Compile one turn's Haskell body (a `result :: M a` expression, wrapped in the
/// mock 10-effect preamble) into Core + its DataConTable.
fn compile_turn(
    harness: &EvalHarness,
    body: &str,
) -> (tidepool_repr::CoreExpr, tidepool_repr::DataConTable) {
    let source = mock::mcp_module(body);
    let compiled = harness
        .compile(&source, "result")
        .unwrap_or_else(|e| panic!("compile failed for turn body {body:?}: {e}"));
    (compiled.expr, compiled.table)
}

/// Bootstrap a resident session whose machine seeds its ConTags from `boot_body`
/// (the first turn's table carries the full 10-effect stack). Handlers are the
/// mock stack (its `MockKv` HashMap is the resident cross-turn accumulator).
fn bootstrap(
    harness: &EvalHarness,
    boot_body: &str,
) -> ResidentSession<AsSink<impl DispatchEffect<()> + Send>, TestSink> {
    let (expr, table) = compile_turn(harness, boot_body);
    let effect_names = mock::EFFECT_NAMES.iter().map(|s| s.to_string()).collect();
    ResidentSession::bootstrap(
        &expr,
        table,
        AsSink(mock::min_stack()),
        ASK_TAG,
        effect_names,
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
    )
    .expect("bootstrap the resident machine")
}

fn setup() -> Option<EvalHarness> {
    if !eval_harness::extract_available() {
        eprintln!("Skipping: tidepool-extract toolchain not available (run inside `nix develop`)");
        return None;
    }
    // The mock preamble imports `Tidepool.Prelude`/`Tidepool.Aeson.KeyMap`,
    // so the stdlib `lib/` must be on the include path (`.with_stdlib()`); the
    // self-contained GADT preamble means NO `.tidepool/lib` verb-library
    // dependency.
    Some(EvalHarness::new().with_stdlib())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The DONE criterion, end to end: a resident session runs a turn that suspends
/// at `ask`, the eval thread exits (nothing parked), a resume with a value
/// completes the turn on a FRESH thread, the machine is back in its slot, and a
/// subsequent turn sees state the suspend/resume turn wrote (resident KV).
#[test]
fn multi_turn_accumulates_across_suspend_resume() {
    let Some(harness) = setup() else { return };
    // Bootstrap from a trivial effectful turn (seeds the 10-effect ConTags).
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");

    // Turn 1: write a KV key, then `ask`, then write a second key using the
    // ANSWER — so completion depends on the resumed value, and BOTH writes land
    // in the resident MockKv across the suspend boundary.
    let (t1_expr, t1_table) = compile_turn(
        &harness,
        "result :: M Int\nresult = do\n  \
           send (KvSet \"before\" (toJSON (1 :: Int)))\n  \
           n <- send (Ask \"pick a number\")\n  \
           send (KvSet \"after\" n)\n  \
           pure (0 :: Int)",
    );
    let outcome = session
        .run(
            "turn1",
            &t1_expr,
            &t1_table,
            &tidepool_codegen::emit::ExternalEnv::new(),
        )
        .expect("turn 1 runs");

    let hole = match outcome {
        ResidentOutcome::Suspended { hole, request, .. } => {
            // The bridged Ask request carries the prompt (already fully forced
            // by the JIT bridge — a plain Text leaf, so an empty table renders
            // it).
            let json = value_to_json(&request, &tidepool_repr::DataConTable::default(), 0);
            assert!(
                json.to_string().contains("pick a number"),
                "suspension request should carry the prompt, got {json}"
            );
            hole
        }
        ResidentOutcome::Completed { .. } => panic!("turn 1 should suspend at `ask`, not complete"),
    };

    // The session is now suspended — no OS thread is parked (E2). A NEW run is
    // rejected cleanly until the ask is resolved.
    let (intrude_expr, intrude_table) =
        compile_turn(&harness, "result :: M Int\nresult = pure (9 :: Int)");
    match session.run(
        "intrude",
        &intrude_expr,
        &intrude_table,
        &tidepool_codegen::emit::ExternalEnv::new(),
    ) {
        Err(ResidentError::Suspended(h)) => assert_eq!(h, hole),
        other => panic!("a suspended session must reject a new run; got {other:?}"),
    }

    // Resume on the WRONG continuation id: rejected WITHOUT consuming the pending
    // one (atomic validate-before-consume).
    match session.resume("scont_does_not_exist", int(7)) {
        Err(ResidentError::WrongContinuation { attempted, pending }) => {
            assert_eq!(attempted, "scont_does_not_exist");
            assert_eq!(pending.as_deref(), Some(hole.as_str()));
        }
        other => panic!("wrong-id resume must not consume the continuation; got {other:?}"),
    }
    assert_eq!(
        session.pending_continuation(),
        Some(hole.as_str()),
        "the pending continuation survives a rejected resume"
    );

    // Resume with the real answer (42): the turn completes on a fresh thread,
    // and the machine returns to its slot (idle).
    match session
        .resume(&hole, int(42))
        .expect("resume with the answer")
    {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(0));
        }
        ResidentOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
    }
    assert!(
        session.is_idle(),
        "the session is idle after the resumed turn completes"
    );

    // Turn 2 (machine REUSE after a resumed turn): read back BOTH keys the
    // suspend/resume turn wrote. "before" proves the pre-suspend write survived;
    // "after" proves the resumed value (42) landed post-resume. This is the
    // resident cross-turn state.
    let (t2_expr, t2_table) = compile_turn(
        &harness,
        "result :: M Value\nresult = do\n  \
           b <- send (KvGet \"before\")\n  \
           a <- send (KvGet \"after\")\n  \
           pure (toJSON [b, a])",
    );
    let outcome = session
        .run(
            "turn2",
            &t2_expr,
            &t2_table,
            &tidepool_codegen::emit::ExternalEnv::new(),
        )
        .expect("turn 2 runs on the reused machine");

    match outcome {
        ResidentOutcome::Completed { result, .. } => {
            let json = result.to_json();
            assert_eq!(
                json,
                serde_json::json!([1, 42]),
                "turn 2 must see the resident KV: before=1 (pre-suspend), after=42 (resumed value)"
            );
        }
        ResidentOutcome::Suspended { .. } => panic!("turn 2 should complete"),
    }
}

/// A plain (non-suspending) resident turn completes and returns its value, and
/// a following turn reuses the same machine — the base residency path with no
/// ask involved.
#[test]
fn plain_turns_reuse_the_machine() {
    let Some(harness) = setup() else { return };
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");

    for expected in [11i64, 22, 33] {
        let body = format!("result :: M Int\nresult = pure ({expected} :: Int)");
        let (expr, table) = compile_turn(&harness, &body);
        match session
            .run(
                "plain",
                &expr,
                &table,
                &tidepool_codegen::emit::ExternalEnv::new(),
            )
            .expect("plain turn runs")
        {
            ResidentOutcome::Completed { result, .. } => {
                assert_eq!(result.to_json(), serde_json::json!(expected));
            }
            ResidentOutcome::Suspended { .. } => panic!("a pure turn should not suspend"),
        }
        assert!(session.is_idle());
    }
}
