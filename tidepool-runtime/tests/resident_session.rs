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
    lines: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
}

impl tidepool_runtime::session::OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock())
    }
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().clone()
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
        request: &Value,
        cx: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        let unit_cx = EffectContext::with_user(cx.table(), &());
        self.0.dispatch(request, &unit_cx)
    }
}

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
        effect_names,
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap the resident machine")
}

fn setup() -> EvalHarness {
    eval_harness::require_extract();
    // The mock preamble imports `Tidepool.Prelude`/`Tidepool.Aeson.KeyMap`,
    // so the stdlib `lib/` must be on the include path (`.with_stdlib()`); the
    // self-contained GADT preamble means NO `.tidepool/lib` verb-library
    // dependency.
    EvalHarness::new().with_stdlib()
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
    let harness = setup();
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
        .run("turn1", &t1_expr, &t1_table)
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
    // PARKED PATH: a new top-level run over a
    // parked frame is ORDINARY — it completes while the frame stays parked
    // and rooted. (This inverts the old reject-while-suspended pin.)
    let (intrude_expr, intrude_table) =
        compile_turn(&harness, "result :: M Int\nresult = pure (9 :: Int)");
    match session.run("intrude", &intrude_expr, &intrude_table) {
        Ok(ResidentOutcome::Completed { result, .. }) => {
            assert_eq!(result.to_json(), serde_json::json!(9));
        }
        other => panic!("a run over a parked frame must complete; got {other:?}"),
    }
    assert_eq!(
        session.pending_continuation(),
        Some(hole.cont_id()),
        "the parked hole survives an unrelated run"
    );

    // Resume with the real answer (42): the turn completes on a fresh thread,
    // and the machine returns to its slot (idle). Clone the hole first — it
    // is `ResidentHole`, non-constructible outside this API, so the ONLY way
    // to exercise the "wrong/stale continuation" rejection path below is to
    // resume the SAME hole again after it has already been spent.
    let stale_hole = hole.clone();
    match session
        .resume(hole, int(42))
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

    // Resuming the now-stale (already-consumed) hole again is rejected
    // WITHOUT touching anything else (atomic validate-before-consume) — the
    // session stays idle, exactly as it was left above.
    let stale_id = stale_hole.cont_id().to_string();
    match session.resume(stale_hole, int(7)) {
        Err(ResidentError::WrongContinuation { attempted, pending }) => {
            assert_eq!(attempted, stale_id);
            assert!(pending.is_empty());
        }
        other => panic!("resuming an already-consumed hole must be rejected; got {other:?}"),
    }
    assert!(
        session.is_idle(),
        "a rejected resume on a stale hole leaves the session idle"
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
        .run("turn2", &t2_expr, &t2_table)
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

/// Segment 40 — nested child runs on a SUSPENDED resident session, through the
/// production path (`ResidentSession::run_child` → `run_child_fragment` → the
/// stowed-continuation GC root). A turn suspends at `ask`; while suspended, a
/// CHILD turn runs against the SAME machine (allocating hard enough to force a
/// real GC), the session stays suspended on its hole, and the parent then
/// resumes correctly. This is the DONE criterion: a parent suspended at a typed
/// yield hosts child fragment runs — including GC-forcing ones — and resumes.
#[test]
fn nested_child_runs_while_parent_suspended_then_resumes() {
    let harness = setup();
    // Small nursery so a child's allocation forces a real collection with the
    // parent's continuation stowed and GC-rooted.
    let (expr, table) = compile_turn(&harness, "result :: M Int\nresult = pure (0 :: Int)");
    let effect_names = mock::EFFECT_NAMES.iter().map(|s| s.to_string()).collect();
    let mut session = ResidentSession::bootstrap(
        &expr,
        table,
        AsSink(mock::min_stack()),
        effect_names,
        TestSink::default(),
        Vec::new(),
        1 << 16,
        None,
    )
    .expect("bootstrap");

    // Turn 1: write a KV key, then suspend at `ask`.
    let (t1_expr, t1_table) = compile_turn(
        &harness,
        "result :: M Int\nresult = do\n  \
           send (KvSet \"parent\" (toJSON (7 :: Int)))\n  \
           n <- send (Ask \"pick\")\n  \
           send (KvSet \"answered\" n)\n  \
           pure (0 :: Int)",
    );
    let hole = match session.run("t1", &t1_expr, &t1_table).expect("turn 1 runs") {
        ResidentOutcome::Suspended { hole, .. } => hole,
        ResidentOutcome::Completed { .. } => panic!("turn 1 should suspend at ask"),
    };

    // A CHILD turn runs against the suspended parent. It allocates a sizeable
    // list and folds it (forcing GC in the small nursery), reading the parent's
    // resident KV zero-copy, and returns a derived value. The parent's stowed
    // continuation is GC-rooted throughout.
    let (child_expr, child_table) = compile_turn(
        &harness,
        "result :: M Int\nresult = do\n  \
           p <- send (KvGet \"parent\")\n  \
           let xs = [1 .. 20000 :: Int]\n  \
           pure (sum xs)",
    );
    for round in 0..3 {
        let child = session
            .run_child(
                "child",
                &child_expr,
                &child_table,
                &tidepool_codegen::emit::ExternalEnv::new(),
            )
            .unwrap_or_else(|e| panic!("child run {round} against suspended parent: {e}"));
        assert_eq!(
            child.to_json(),
            serde_json::json!(200010000i64),
            "child computes sum [1..20000] = 200010000 (round {round})"
        );
        // The session is STILL suspended on the same hole after each child.
        assert_eq!(
            session.pending_continuation(),
            Some(hole.cont_id()),
            "the parent stays suspended on its hole across child runs (round {round})"
        );
    }

    // PARKED PATH: a new TOP-LEVEL run over the parked parent is ordinary —
    // it completes, and the parent's hole survives (the frame is a registered
    // root, not a fragile slot).
    let (intrude_expr, intrude_table) =
        compile_turn(&harness, "result :: M Int\nresult = pure (1 :: Int)");
    match session.run("intrude", &intrude_expr, &intrude_table) {
        Ok(ResidentOutcome::Completed { result, .. }) => {
            assert_eq!(result.to_json(), serde_json::json!(1));
        }
        other => panic!("a run over a parked frame must complete; got {other:?}"),
    }
    assert_eq!(
        session.pending_continuation(),
        Some(hole.cont_id()),
        "the parked hole survives an unrelated top-level run"
    );

    // Resume the parent with 42: the continuation (stowed across all the child
    // GCs) drives to completion correctly.
    match session
        .resume(hole, int(42))
        .expect("resume after children")
    {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(0));
        }
        ResidentOutcome::Suspended { .. } => panic!("resume should complete"),
    }
    assert!(session.is_idle());

    // Post-resume: the parent's pre-suspend write AND the resumed value both
    // landed — proving the continuation and the resident state survived the
    // whole suspend → child-GC → resume round-trip.
    let (verify_expr, verify_table) = compile_turn(
        &harness,
        "result :: M Value\nresult = do\n  \
           p <- send (KvGet \"parent\")\n  \
           a <- send (KvGet \"answered\")\n  \
           pure (toJSON [p, a])",
    );
    match session
        .run("verify", &verify_expr, &verify_table)
        .expect("verify turn")
    {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(
                result.to_json(),
                serde_json::json!([7, 42]),
                "resident state survived suspend → child GC → resume"
            );
        }
        ResidentOutcome::Suspended { .. } => panic!("verify turn should complete"),
    }
}

/// A `run_child` on an IDLE (not suspended) resident session is rejected —
/// a nested child requires a suspended parent (segment 40).
#[test]
fn run_child_on_idle_session_is_not_suspended() {
    let harness = setup();
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");
    let (expr, table) = compile_turn(&harness, "result :: M Int\nresult = pure (1 :: Int)");
    match session.run_child(
        "child",
        &expr,
        &table,
        &tidepool_codegen::emit::ExternalEnv::new(),
    ) {
        Err(ResidentError::NotSuspended) => {}
        other => panic!("run_child on an idle session must be NotSuspended; got {other:?}"),
    }
}

/// Codex review item 9 (HIGH, `codex-review-2026-08-08.md`): a resume answer
/// that fails the JIT's A5 NF-force (a bottom/unforced-thunk answer,
/// `jit_machine.rs`'s `resume_suspended_inner` around :1328) must NOT wedge
/// the session. The machine deliberately leaves the continuation stowed on
/// that rejection so the caller can retry with a corrected answer —
/// `ResidentSession` must track that, not blindly clear `pending` before the
/// machine is even called. Before the fix this wedged: `pending` cleared
/// unconditionally in `reenter`, `is_idle()` lied `true`, a corrected resume
/// was rejected as `WrongContinuation` (pending already gone), and a
/// subsequent top-level turn would panic on the machine's stowed-
/// continuation `assert!` (caught by `catch_unwind` and surfaced as a run
/// error here, not a raw panic — see `on_eval_thread`).
#[test]
fn retryable_resume_failure_does_not_wedge_the_session() {
    let harness = setup();
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");

    let (t1_expr, t1_table) = compile_turn(
        &harness,
        "result :: M Value\nresult = do\n  \
           n <- send (Ask \"pick a number\")\n  \
           pure n",
    );
    let hole = match session.run("t1", &t1_expr, &t1_table).expect("turn 1 runs") {
        ResidentOutcome::Suspended { hole, .. } => hole,
        ResidentOutcome::Completed { .. } => panic!("turn 1 should suspend at `ask`"),
    };

    // A bottom-bearing answer (an unforced thunk reference): the JIT's A5
    // NF-force rejects it WITHOUT consuming the stowed continuation.
    let bottom = Value::ThunkRef(tidepool_eval::value::ThunkId(999));
    match session.resume(hole.clone(), bottom) {
        Err(_) => {}
        Ok(outcome) => panic!("a bottom-bearing answer must be rejected; got {outcome:?}"),
    }

    // (a) The session must still report suspended, not idle — the machine
    // did not consume the continuation on a retryable rejection.
    assert!(
        !session.is_idle(),
        "a retryable resume failure must leave the session suspended, not idle"
    );
    assert_eq!(
        session.pending_continuation(),
        Some(hole.cont_id()),
        "the same hole stays pending after a retryable rejection"
    );

    // (b) A corrected resume on the SAME hole must succeed.
    match session
        .resume(hole, int(42))
        .expect("corrected resume on the same hole succeeds")
    {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(42));
        }
        ResidentOutcome::Suspended { .. } => panic!("corrected resume should complete"),
    }
    assert!(
        session.is_idle(),
        "the session is idle after the corrected resume completes"
    );

    // (c) A subsequent top-level turn must run cleanly to completion — no
    // stowed-continuation assertion failure from the earlier rejection.
    let (t2_expr, t2_table) = compile_turn(&harness, "result :: M Int\nresult = pure (7 :: Int)");
    match session.run("t2", &t2_expr, &t2_table) {
        Ok(ResidentOutcome::Completed { result, .. }) => {
            assert_eq!(result.to_json(), serde_json::json!(7));
        }
        other => panic!("post-recovery turn must complete cleanly; got {other:?}"),
    }
}

/// A plain (non-suspending) resident turn completes and returns its value, and
/// a following turn reuses the same machine — the base residency path with no
/// ask involved.
#[test]
fn plain_turns_reuse_the_machine() {
    let harness = setup();
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");

    for expected in [11i64, 22, 33] {
        let body = format!("result :: M Int\nresult = pure ({expected} :: Int)");
        let (expr, table) = compile_turn(&harness, &body);
        match session
            .run("plain", &expr, &table)
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
