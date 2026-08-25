//! THE REPRESENTATION-PINNING TEST: **two green threads blocked on two
//! DIFFERENT effects have BOTH holes pending in the
//! session's multi-hole registry SIMULTANEOUSLY, and resuming them in
//! EITHER order produces identical results.**
//!
//! Exercises [`ResidentSession::run_forked`] directly — the green-thread
//! fork entry — rather than going through the driver's `Green` scheduler
//! (`tidepool-harness/src/selfharness/driver.rs`), so this is the substrate
//! claim alone: a hand-built program suspends carrying a closure at field 1
//! of its request (the SAME sentinel-tenure mechanism `finalize` already
//! uses, `finalized_handle`), and that closure is forked as a NEW
//! suspension-capable top-level run under its own realm. No GHC extract is
//! needed — every `CoreExpr` here is hand-built, the same style
//! `tidepool-codegen/tests/realm_handles.rs` uses for the codegen layer.
//!
//! Assertion is on the registry's PENDING SET
//! ([`ResidentSession::parked_holes`]), not a completion count — a count
//! passes under a representation that secretly serializes the two threads,
//! exactly the bug this test exists to catch.

use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};
use tidepool_runtime::session::{OutputSink, ResidentHole, ResidentOutcome, ResidentSession};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;

use tidepool_codegen::jit_machine::RealmId;

// ─── freer-simple constructor IDs (qualified names are what the machine's
// ConTags setup resolves by; the numeric ids here are local to this file's
// own table, same convention `realm_handles.rs` uses) ───────────────────
const VAL_ID: DataConId = DataConId(1);
const E_ID: DataConId = DataConId(2);
const UNION_ID: DataConId = DataConId(3);
const LEAF_ID: DataConId = DataConId(4);
const NODE_ID: DataConId = DataConId(5);

/// The scratch "spawn wrapper" Con: `SpawnWrap dummy closure` — field 1 is
/// the thread body closure, the sentinel-tenure mechanism's load-bearing
/// field position (any constructor identity works; it keys on POSITION and
/// closure-ness, never the name).
const WRAP_ID: DataConId = DataConId(6);
/// A thread body's own completion wrapper: `ThreadResult answer`.
const RESULT_ID: DataConId = DataConId(7);
/// The scratch wrapper suspension's own (unused) completion wrapper.
const OUTER_ID: DataConId = DataConId(8);

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    for (id, name, tag, arity, qualified) in [
        (VAL_ID, "Val", 0u32, 1u32, Some("Control.Monad.Freer.Val")),
        (E_ID, "E", 0, 2, Some("Control.Monad.Freer.E")),
        (UNION_ID, "Union", 0, 2, Some("Data.OpenUnion.Union")),
        (LEAF_ID, "Leaf", 0, 1, Some("Data.FTCQueue.Leaf")),
        (NODE_ID, "Node", 0, 2, Some("Data.FTCQueue.Node")),
        (WRAP_ID, "SpawnWrap", 1, 2, None),
        (RESULT_ID, "ThreadResult", 1, 1, None),
        (OUTER_ID, "WrapResult", 1, 1, None),
    ] {
        table.insert(DataCon {
            id,
            name: name.to_string(),
            tag,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: qualified.map(str::to_string),
            type_name: String::new(),
        });
    }
    table
}

/// Build a program that suspends on `wrap_tag` carrying
/// `SpawnWrap dummy bodyClosure`, where `bodyClosure` is `\_ignored -> E
/// (Union body_tag body_lit) (Leaf (\v -> Val (ThreadResult v)))` — i.e. a
/// thread body that, once `run_forked` applies it, immediately suspends on
/// `body_tag`/`body_lit` and completes with `ThreadResult answer` on resume.
/// Mirrors `realm_handles.rs`'s `build_finalize_suspend`, generalized so the
/// closure field is a genuine suspending body instead of the identity.
fn build_wrap_suspend(wrap_tag: u64, dummy: i64, body_tag: u64, body_lit: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    // The thread body: `\_ -> E (Union body_tag body_lit) (Leaf (\v -> Val
    // (ThreadResult v)))`. The arg binder is never referenced (a spawn body
    // ignores its dummy `Int` — `AsyncSpawnWith`'s own shape).
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let body_req_lit = b.push(CoreFrame::Lit(Literal::LitInt(body_lit)));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, body_req_lit],
    });
    let cont_v = b.push(CoreFrame::Var(CONT_VAR));
    let cont_result = b.push(CoreFrame::Con {
        tag: RESULT_ID,
        fields: vec![cont_v],
    });
    let cont_val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![cont_result],
    });
    let cont_lam = b.push(CoreFrame::Lam {
        binder: CONT_VAR,
        body: cont_val,
    });
    let body_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![cont_lam],
    });
    let body_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![body_union, body_leaf],
    });
    let body_closure = b.push(CoreFrame::Lam {
        binder: ARG_VAR,
        body: body_e,
    });

    // The scratch wrapper suspend: `E (Union wrap_tag (SpawnWrap dummy
    // bodyClosure)) (Leaf (\v -> Val (WrapResult v)))`.
    const OUTER_CONT_VAR: VarId = VarId(2);
    let dummy_lit = b.push(CoreFrame::Lit(Literal::LitInt(dummy)));
    let wrap_con = b.push(CoreFrame::Con {
        tag: WRAP_ID,
        fields: vec![dummy_lit, body_closure],
    });
    let wrap_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(wrap_tag)));
    let wrap_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![wrap_tag_lit, wrap_con],
    });
    let outer_cont_v = b.push(CoreFrame::Var(OUTER_CONT_VAR));
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_cont_v],
    });
    let outer_val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![outer_result],
    });
    let outer_lam = b.push(CoreFrame::Lam {
        binder: OUTER_CONT_VAR,
        body: outer_val,
    });
    let outer_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![outer_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });
    b.build()
}

fn expect_int(v: &Value) -> i64 {
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        other => panic!("expected a LitInt, got {other:?}"),
    }
}

/// Unwrap `ThreadResult answer` (a thread's completion shape).
fn expect_thread_result(v: &Value) -> i64 {
    match v {
        Value::Con(id, fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
            expect_int(&fields[0])
        }
        other => panic!("expected ThreadResult(answer), got {other:?}"),
    }
}

#[derive(Clone, Default)]
struct TestSink {
    lines: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
}

impl OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock())
    }
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().clone()
    }
}

/// Never dispatches — every tag this file constructs is >= the suspend
/// threshold (0), so everything suspends; a real dispatch call would mean
/// the representation under test silently stopped suspending.
struct NoDispatch;
impl DispatchEffect<TestSink> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — expected a suspension");
    }
}

fn fresh_session() -> ResidentSession<NoDispatch, TestSink> {
    ResidentSession::unbootstrapped(
        NoDispatch,
        0, // ask_tag: suspend threshold 0 — every tag here suspends.
        Vec::new(),
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
}

/// Fork one green thread: run the scratch wrapper suspension, take its
/// tenured body as a handle (`finalized_handle` — the same call the
/// finalize-by-reference path uses), retire the scratch hole immediately
/// (spawner-continues-first, mirroring the driver's own discipline), then
/// `run_forked` the body under `realm`. Returns the thread's OWN pending
/// hole (parked on `body_tag`/`body_lit`).
#[allow(clippy::too_many_arguments)]
fn spawn_thread(
    session: &mut ResidentSession<NoDispatch, TestSink>,
    table: &DataConTable,
    label: &str,
    wrap_tag: u64,
    dummy: i64,
    body_tag: u64,
    body_lit: i64,
    realm: RealmId,
) -> ResidentHole {
    let expr = build_wrap_suspend(wrap_tag, dummy, body_tag, body_lit);
    let outcome = session
        .run(label, &expr, table)
        .unwrap_or_else(|e| panic!("{label}: wrap suspend run failed: {e}"));
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("{label}: wrap suspend must suspend, got {other:?}"),
    };
    let handle = session
        .finalized_handle(wrap_hole.cont_id())
        .unwrap_or_else(|| panic!("{label}: wrap frame carries no untaken body closure"));
    let _ = session
        .resume(wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("{label}: wrap suspend failed to resume to completion: {e}"));
    let outcome = session
        .run_forked(&format!("{label}_thread"), handle, realm, Some(table))
        .unwrap_or_else(|e| panic!("{label}: run_forked failed: {e}"));
    match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("{label}: thread body must suspend on its own effect, got {other:?}"),
    }
}

/// Resume a thread's pending hole with `answer`, returning its
/// `ThreadResult` payload.
fn complete_thread(
    session: &mut ResidentSession<NoDispatch, TestSink>,
    hole: &ResidentHole,
    answer: i64,
) -> i64 {
    let outcome = session
        .resume(hole.clone(), Value::Lit(Literal::LitInt(answer)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    match outcome {
        ResidentOutcome::Completed { result, .. } => expect_thread_result(&result.into_value()),
        other => panic!("thread must complete on its AsyncDoneWith-shaped resume, got {other:?}"),
    }
}

/// Spawn two threads under two DIFFERENT realms, each blocked on a
/// DIFFERENT effect (a different union tag), in the SAME spawn order every
/// time — the two resume-order legs below vary only in which hole they
/// resume first.
fn spawn_two(
    session: &mut ResidentSession<NoDispatch, TestSink>,
    table: &DataConTable,
) -> (ResidentHole, ResidentHole) {
    let hole_a = spawn_thread(session, table, "a", 100, 1, 200, 11, RealmId(1));
    let hole_b = spawn_thread(session, table, "b", 101, 2, 201, 22, RealmId(2));
    (hole_a, hole_b)
}

#[test]
fn two_green_threads_pend_simultaneously_and_resume_order_is_free() {
    let table = table();

    // --- Representation pin: both holes pending in the registry AT ONCE. ---
    let mut session = fresh_session();
    let (hole_a, hole_b) = spawn_two(&mut session, &table);
    let mut pending = session.parked_holes();
    pending.sort_unstable();
    let mut expected = vec![hole_a.cont_id(), hole_b.cont_id()];
    expected.sort_unstable();
    assert_eq!(
        pending, expected,
        "both green threads' holes must be pending in the registry simultaneously — \
         asserted on the pending SET, not a completion count (a count passes under a \
         representation that secretly serializes the two threads)"
    );

    // --- Order 1: resume A, then B. ---
    let a_first = complete_thread(&mut session, &hole_a, 999);
    let b_first = complete_thread(&mut session, &hole_b, 888);

    // --- Order 2: an INDEPENDENT, identically-constructed session, resumed
    // B then A. ---
    let mut session2 = fresh_session();
    let (hole_a2, hole_b2) = spawn_two(&mut session2, &table);
    let b_second = complete_thread(&mut session2, &hole_b2, 888);
    let a_second = complete_thread(&mut session2, &hole_a2, 999);

    assert_eq!(
        a_first, a_second,
        "thread A's result must not depend on resume order"
    );
    assert_eq!(
        b_first, b_second,
        "thread B's result must not depend on resume order"
    );
    assert_eq!(a_first, 999);
    assert_eq!(b_first, 888);
}

// ─── Nested fork — what is sound, and what the open bug is NOT ──────────
//
// A green thread whose body itself forks another thread. Not an edge case:
// node residency IS this shape ("an interior node is a green thread
// whose body, after forking children, is a select loop"), so `forkNode` on an
// interior node depends on it.
//
// Through the full GHC pipeline this reports as GC-forwarding corruption
// ("tag=255 Forwarded"), bisection-confirmed by removing the nesting. The
// test below reproduces the nesting's STRUCTURE without GHC and PASSES,
// including under `TIDEPOOL_GC_POISON=1 TIDEPOOL_HEAP_VERIFY=1`. That is a
// narrowing result, not a contradiction:
//
//   RULED OUT — the fork crossing itself. Tenuring a closure off a frame that
//   `run_forked` created, while that frame is still parked, and forking it
//   under a second realm is sound. So `run_forked`, the sentinel-tenure step,
//   `finalized_handle` on a thread's own frame, and multi-level realm nesting
//   are all fine.
//
//   STILL SUSPECT — a collection RUNNING while nested frames are parked. The
//   bodies here are hand-built and allocate almost nothing, so no GC occurs;
//   the failing pipeline case (`mapConcurrently` over recursive sums)
//   allocates heavily. Injecting a collection at the suspicious point needs a
//   force-GC entry the machine does not expose publicly, which is
//   `tidepool-codegen`'s territory, not this lane's.
//
// So this test is a standing pin on the crossing's structure — it would catch
// a regression there — and is explicitly NOT a reproducer for the open bug.

/// Like [`build_wrap_suspend`], but the thread body's own effect is ANOTHER
/// wrap-suspend carrying `innerClosure` at field 1 — the nested-fork shape.
fn build_nested_wrap_suspend(
    outer_wrap_tag: u64,
    outer_dummy: i64,
    inner_wrap_tag: u64,
    inner_dummy: i64,
    leaf_tag: u64,
    leaf_lit: i64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();

    // innerClosure = `\_ -> E (Union leaf_tag leaf_lit) (Leaf (\v -> Val (ThreadResult v)))`
    const INNER_ARG: VarId = VarId(11);
    const INNER_CONT: VarId = VarId(12);
    let leaf_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(leaf_tag)));
    let leaf_req_lit = b.push(CoreFrame::Lit(Literal::LitInt(leaf_lit)));
    let leaf_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![leaf_tag_lit, leaf_req_lit],
    });
    let ic_v = b.push(CoreFrame::Var(INNER_CONT));
    let ic_res = b.push(CoreFrame::Con {
        tag: RESULT_ID,
        fields: vec![ic_v],
    });
    let ic_val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![ic_res],
    });
    let ic_lam = b.push(CoreFrame::Lam {
        binder: INNER_CONT,
        body: ic_val,
    });
    let ic_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![ic_lam],
    });
    let inner_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![leaf_union, ic_leaf],
    });
    let inner_closure = b.push(CoreFrame::Lam {
        binder: INNER_ARG,
        body: inner_e,
    });

    // bodyClosure = `\_ -> E (Union inner_wrap_tag (SpawnWrap inner_dummy innerClosure))
    //                        (Leaf (\v -> Val (ThreadResult v)))`
    const BODY_ARG: VarId = VarId(13);
    const BODY_CONT: VarId = VarId(14);
    let inner_dummy_lit = b.push(CoreFrame::Lit(Literal::LitInt(inner_dummy)));
    let inner_wrap_con = b.push(CoreFrame::Con {
        tag: WRAP_ID,
        fields: vec![inner_dummy_lit, inner_closure],
    });
    let inner_wrap_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(inner_wrap_tag)));
    let inner_wrap_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![inner_wrap_tag_lit, inner_wrap_con],
    });
    let bc_v = b.push(CoreFrame::Var(BODY_CONT));
    let bc_res = b.push(CoreFrame::Con {
        tag: RESULT_ID,
        fields: vec![bc_v],
    });
    let bc_val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![bc_res],
    });
    let bc_lam = b.push(CoreFrame::Lam {
        binder: BODY_CONT,
        body: bc_val,
    });
    let bc_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![bc_lam],
    });
    let body_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![inner_wrap_union, bc_leaf],
    });
    let body_closure = b.push(CoreFrame::Lam {
        binder: BODY_ARG,
        body: body_e,
    });

    // The scratch outer wrapper, exactly as `build_wrap_suspend`'s.
    const OUTER_CONT: VarId = VarId(15);
    let dummy_lit = b.push(CoreFrame::Lit(Literal::LitInt(outer_dummy)));
    let wrap_con = b.push(CoreFrame::Con {
        tag: WRAP_ID,
        fields: vec![dummy_lit, body_closure],
    });
    let wrap_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(outer_wrap_tag)));
    let wrap_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![wrap_tag_lit, wrap_con],
    });
    let oc_v = b.push(CoreFrame::Var(OUTER_CONT));
    let oc_res = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![oc_v],
    });
    let oc_val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![oc_res],
    });
    let oc_lam = b.push(CoreFrame::Lam {
        binder: OUTER_CONT,
        body: oc_val,
    });
    let oc_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![oc_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, oc_leaf],
    });
    b.build()
}

/// A green thread forks a SECOND green thread from inside its own body — the
/// nested-fork STRUCTURE, with no GC pressure. Passes; see the section
/// comment above for what that does and does not establish.
///
/// Runs under `--ignore-default-filter -p tidepool-runtime`; pair with
/// `TIDEPOOL_GC_POISON=1 TIDEPOOL_HEAP_VERIFY=1` (also green today).
#[test]
fn a_green_thread_can_fork_another_green_thread() {
    let table = table();
    let mut session = fresh_session();

    // Level 0: the scratch spawner, exactly as the flat case.
    let expr = build_nested_wrap_suspend(100, 1, 101, 2, 200, 77);
    let outcome = session
        .run("nested", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };
    let outer_body = session
        .finalized_handle(wrap_hole.cont_id())
        .expect("wrap frame carries the outer thread body");
    session
        .resume(wrap_hole, Value::Lit(Literal::LitInt(0)))
        .expect("scratch spawner resumes");

    // Level 1: the outer thread. Its own body suspends carrying ANOTHER
    // closure at field 1 — the nested `async`.
    let outer_hole = match session
        .run_forked("outer_thread", outer_body, RealmId(1), Some(&table))
        .expect("outer thread forks")
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("outer thread must suspend on its nested spawn, got {other:?}"),
    };

    // Level 2: THE STEP UNDER TEST — tenure the inner body off a frame that
    // `run_forked` itself created, while that frame is still parked, and fork
    // it under its own realm.
    let inner_body = session
        .finalized_handle(outer_hole.cont_id())
        .expect("the outer THREAD's frame must carry its nested spawn's closure");
    session
        .resume(outer_hole, Value::Lit(Literal::LitInt(0)))
        .expect("outer thread resumes past its spawn");
    let inner_hole = match session
        .run_forked("inner_thread", inner_body, RealmId(2), Some(&table))
        .expect("inner thread forks from inside the outer thread")
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("inner thread must suspend on its own effect, got {other:?}"),
    };

    let answer = complete_thread(&mut session, &inner_hole, 77);
    assert_eq!(
        answer, 77,
        "the nested thread's result must survive delivery"
    );
}
