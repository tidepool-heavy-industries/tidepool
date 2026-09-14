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

use tidepool_bridge::Value;
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{request_constructor, DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_repr::{Generation, Literal, PrincipalId, SessionModule};
use tidepool_runtime::session::{
    BoundBinder, ResidentError, ResidentOutcome, ResidentSession, SessionError, SessionRunContext,
    ValueTier,
};
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
        // These tests exercise the resident suspension boundary, not the
        // mock harness's canned Ask response. Routing is nominal now: leave
        // Ask unhandled and let HandleOrSuspend park it. Every other request
        // still goes through the ordinary mock stack.
        if request_constructor(request, cx.table()).rsplit('.').next() == Some("Ask") {
            return Ok(None);
        }
        let unit_cx = EffectContext::with_principal(cx.table(), cx.principal(), &());
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

/// Bootstrap a resident session whose machine seeds its constructors from
/// `boot_body`. Handlers are the mock stack (its `MockKv` HashMap is the
/// resident cross-turn accumulator), except that nominal `Ask` is deliberately
/// left for the suspension boundary.
fn bootstrap(
    harness: &EvalHarness,
    boot_body: &str,
) -> ResidentSession<AsSink<impl DispatchEffect<()> + Send>, TestSink> {
    let (expr, table) = compile_turn(harness, boot_body);
    let mut session = ResidentSession::bootstrap(
        &expr,
        table,
        AsSink(mock::min_stack()),
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap the resident machine");
    session.set_effect_execution(EffectRunPolicy::HandleOrSuspend, LivePayloadPolicy::None);
    session
}

fn setup() -> EvalHarness {
    eval_harness::require_extract();
    // The mock preamble imports `Tidepool.Prelude`/`Tidepool.Aeson.KeyMap`,
    // so the stdlib `lib/` must be on the include path (`.with_stdlib()`); the
    // self-contained GADT preamble means NO `.tidepool/lib` verb-library
    // dependency.
    EvalHarness::new().with_stdlib()
}

fn binder(name: &str, var_id: u64, generation: Generation) -> BoundBinder {
    BoundBinder {
        name: name.to_string(),
        var_id,
        module: SessionModule::val(generation).module_name(),
        tier: ValueTier::Tier0Data,
        type_display: "Int".to_string(),
    }
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
        ResidentOutcome::BindingsCommitted { .. } => {
            panic!("turn 1 should suspend at `ask`, not bind names")
        }
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
        ResidentOutcome::BindingsCommitted { .. } => {
            panic!("a value-returning resume must not report a projected binding")
        }
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
        ResidentOutcome::BindingsCommitted { .. } => {
            panic!("a value-returning turn must not report a projected binding")
        }
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
    let mut session = ResidentSession::bootstrap(
        &expr,
        table,
        AsSink(mock::min_stack()),
        TestSink::default(),
        Vec::new(),
        1 << 16,
        None,
    )
    .expect("bootstrap");
    session.set_effect_execution(EffectRunPolicy::HandleOrSuspend, LivePayloadPolicy::None);

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
        ResidentOutcome::BindingsCommitted { .. } => {
            panic!("turn 1 should suspend at ask, not bind names")
        }
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
        ResidentOutcome::BindingsCommitted { .. } => {
            panic!("a value-returning resume must not report a projected binding")
        }
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
        ResidentOutcome::BindingsCommitted { .. } => {
            panic!("a value-returning turn must not report a projected binding")
        }
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
            ResidentOutcome::BindingsCommitted { .. } => {
                panic!("a pure value turn must not report a projected binding")
            }
        }
        assert!(session.is_idle());
    }
}

#[test]
fn resumed_bind_keeps_its_originating_resource_and_lexical_scopes() {
    let harness = setup();
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");
    let scope_one = session.mint_scope(ScopeId::ROOT).expect("mint scope one");
    let scope_two = session.mint_scope(ScopeId::ROOT).expect("mint scope two");
    let realm_one = RealmId::fresh();
    let realm_two = RealmId::fresh();
    let context_one = SessionRunContext::new(realm_one, scope_one, PrincipalId::new(1, 1));
    let context_two = SessionRunContext::new(realm_two, scope_two, PrincipalId::new(2, 1));
    session
        .set_run_context(context_one)
        .expect("select origin context");

    let generation = session.val_gen().next();
    let bound = binder("answer", (0xFE << 56) | 101, generation);
    let (expr, table) = compile_turn(
        &harness,
        "result :: M Int\nresult = do\n  _ <- send (Ask \"first\")\n  _ <- send (Ask \"second\")\n  pure (41 :: Int)",
    );
    let first_hole = match session
        .run_bind("scoped_bind", &expr, &table, &bound, generation)
        .expect("bind parks at its first ask")
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("bind must suspend, got {other:?}"),
    };
    assert_eq!(session.parked_realm(&first_hole), Some(realm_one));

    session
        .set_run_context(context_two)
        .expect("select ambient context");
    let second_hole = match session
        .resume(first_hole, int(0))
        .expect("first resume re-suspends")
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("first resume must re-suspend, got {other:?}"),
    };
    assert_eq!(
        session.parked_realm(&second_hole),
        Some(realm_one),
        "the frame's realm survives re-suspension instead of adopting the ambient realm"
    );

    match session
        .resume(second_hole, int(0))
        .expect("second resume completes")
    {
        ResidentOutcome::Completed { .. } => {}
        other => panic!("second resume must complete the bind, got {other:?}"),
    }
    assert!(session.current_binding_in(scope_one, "answer").is_some());
    assert!(session.current_binding_in(scope_two, "answer").is_none());

    session
        .set_run_context(context_one)
        .expect("restore origin context");
    let failed_generation = session.val_gen().next();
    let failed_bound = binder("orphan", (0xFE << 56) | 104, failed_generation);
    let (failed_expr, failed_table) = compile_turn(
        &harness,
        "result :: M Int\nresult = do\n  _ <- send (Ask \"fail\")\n  pure (42 :: Int)",
    );
    let failed_hole = match session
        .run_bind(
            "failed_scoped_bind",
            &failed_expr,
            &failed_table,
            &failed_bound,
            failed_generation,
        )
        .expect("second bind parks")
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("second bind must suspend, got {other:?}"),
    };
    session
        .set_run_context(context_two)
        .expect("restore ambient context");
    session.retire_scope(scope_one);
    let result = session.resume(failed_hole, int(0));
    assert!(
        matches!(result, Err(ResidentError::Session(SessionError::DeadScope(scope))) if scope == scope_one),
        "materialization must reject its retired initiating scope, got {result:?}"
    );
    assert!(
        session.is_idle(),
        "the consumed frame must not leave a stale hole"
    );
    assert_eq!(session.close_realm(realm_two), (0, 0));
    assert_eq!(
        session.close_realm(realm_one),
        (0, 1),
        "the unadopted completion handle remains in the frame's originating realm"
    );
}

#[test]
fn resumed_projected_bind_keeps_its_originating_lexical_scope() {
    let harness = setup();
    let mut session = bootstrap(&harness, "result :: M Int\nresult = pure (0 :: Int)");
    let scope_one = session.mint_scope(ScopeId::ROOT).expect("mint scope one");
    let scope_two = session.mint_scope(ScopeId::ROOT).expect("mint scope two");
    let realm_one = RealmId::fresh();
    let realm_two = RealmId::fresh();
    session
        .set_run_context(SessionRunContext::new(
            realm_one,
            scope_one,
            PrincipalId::new(1, 1),
        ))
        .expect("select origin context");

    let generation = session.val_gen().next();
    let binders = [
        binder("left", (0xFE << 56) | 102, generation),
        binder("right", (0xFE << 56) | 103, generation),
    ];
    let (expr, table) = compile_turn(
        &harness,
        "result :: M (Int, Int)\nresult = do\n  _ <- send (Ask \"project\")\n  pure (41 :: Int, 42 :: Int)",
    );
    let hole = match session
        .run_projected_bind_with_sites("scoped_project", &expr, &table, &binders, generation, &[])
        .expect("projected bind parks")
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("projected bind must suspend, got {other:?}"),
    };

    session
        .set_run_context(SessionRunContext::new(
            realm_two,
            scope_two,
            PrincipalId::new(2, 1),
        ))
        .expect("select ambient context");
    match session
        .resume(hole, int(0))
        .expect("projected bind completes")
    {
        ResidentOutcome::BindingsCommitted { .. } => {}
        other => panic!("projected bind must commit its fields, got {other:?}"),
    }
    assert!(session.current_binding_in(scope_one, "left").is_some());
    assert!(session.current_binding_in(scope_one, "right").is_some());
    assert!(session.current_binding_in(scope_two, "left").is_none());
    assert!(session.current_binding_in(scope_two, "right").is_none());
}
