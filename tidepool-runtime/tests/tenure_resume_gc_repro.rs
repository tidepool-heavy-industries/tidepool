//! Injected-collection reproducers for the tenure-then-resume rooting gap
//! (PRD 20 S1-L4; see `tidepool-runtime/tests/nested_async_repro.rs`'s
//! module doc for the full mechanism writeup, and
//! `tidepool-harness/tests/fixtures/MinimalWatchListHarness.hs` for the
//! smallest known GHC-driven reproducer).
//!
//! No GHC, no nesting, no loop: every test here hand-builds a suspend whose
//! request carries a field-1 CLOSURE (`SpawnWrap dummy bodyClosure`, the
//! same load-bearing field-1 position `AsyncSpawnWith` uses) that gets
//! sentinel-tenured at suspend, using `ResidentSession::force_gc_for_test`
//! (the diagnostic instrument this file's investigation added — see
//! `tidepool-codegen/CLAUDE.md`'s diagnostics table) to inject a real
//! collection into the window between tenure and resume.
//!
//! # Root cause found and FIXED — `OldSpace::tenure` folds in a full minor GC
//!
//! Thirteen structural variants, all passing, unweakened, under
//! `TIDEPOOL_GC_POISON=1 TIDEPOOL_HEAP_VERIFY=1` — escalating in fidelity all
//! the way to the REAL `Tidepool.Node.forkNode`/`MinimalWatchListHarness.hs`
//! shape.
//!
//! Root cause (found via `shared_free_variable_stale_immediately_after_tenure_with_no_intervening_gc`,
//! repro #13, last in this file): every one of the first twelve tests calls
//! `force_gc_for_test` at least once between tenure and the later read — that
//! shared ingredient was load-bearing, not incidental. `OldSpace::tenure`'s
//! own `cheney_copy` call (`tidepool-codegen/src/old_space.rs`) walked ONLY
//! the tenure root's transitive graph — a single-element root slice.
//! `raw::evacuate` physically overwrites the moved value's old nursery
//! address with a `TAG_FORWARDED` stub the instant it is copied. A SIBLING
//! object (like the wrap continuation in repro #13's shape) that
//! independently captured a pointer to that same value, but is not reachable
//! from the tenure root, was never visited by that walk — its own field
//! still held the pre-tenure address, which now reads as a forwarding stub.
//! The stub only got fixed up as a side effect of a LATER collection that
//! happened to include the sibling in its own root set — and nothing
//! guaranteed one would run before the sibling's field was read.
//!
//! Fix: `OldSpace::tenure` now folds a real minor collection over every
//! ordinary root category (frame-walked stack, Rust roots, persistent roots,
//! stowed roots, remembered slots, VMContext tail-call slots) into every
//! tenure call that actually evacuates something
//! (`run_minor_collection_for_tenure_fixup`, `tidepool-codegen/src/host_fns/gc.rs`),
//! immediately after its own tenure-root walk. This reuses the SAME
//! forward-following logic already proven correct for shared substructure
//! across two separate tenure calls (`old_space.rs`'s
//! `test_overlapping_tenures_preserve_sharing`) — a sibling's stale field
//! gets fixed as a normal consequence of that pass walking an ordinary root
//! that reaches it, not by a new bespoke mechanism. See
//! `run_minor_collection_for_tenure_fixup`'s doc for why this is safe to run
//! from every tenure call site (re-entrancy, frame-pointer provenance, and
//! why it must not touch `gc_trigger`'s call-count instrumentation), and
//! this lane's `notify_parent` history for the full diagnosis.

use tidepool_codegen::jit_machine::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};
use tidepool_runtime::session::{OutputSink, ResidentOutcome, ResidentSession};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;

const VAL_ID: DataConId = DataConId(1);
const E_ID: DataConId = DataConId(2);
const UNION_ID: DataConId = DataConId(3);
const LEAF_ID: DataConId = DataConId(4);
const NODE_ID: DataConId = DataConId(5);
const RESULT_ID: DataConId = DataConId(7);
/// The scratch "spawn wrapper" Con: `SpawnWrap dummy closure` — same
/// load-bearing field-1 position `nested_async_repro.rs`/`green_thread_representation.rs`
/// use (sentinel-tenure keys on POSITION and closure-ness, never the name).
const WRAP_ID: DataConId = DataConId(6);
/// A thread body's own completion wrapper: `ThreadResult answer`.
const OUTER_ID: DataConId = DataConId(8);
/// `Pair a b` — carries the shared free variable alongside whatever else a
/// continuation produces, so its value is directly observable in the result.
const PAIR_ID: DataConId = DataConId(9);
/// `Event a = Event { eventWatches :: [Watch], eventProject :: r -> Maybe a }`
/// (`tidepool-mcp/src/effect_defs.rs`'s generated shape) — a 2-field Con
/// whose fields are a LIST and a CLOSURE.
const EVENT_ID: DataConId = DataConId(10);
/// `WatchMailbox mid` — one list element.
const WATCH_ID: DataConId = DataConId(11);
/// `(:)` — cons cell, 2 fields (head, tail).
const CONS_ID: DataConId = DataConId(12);
/// `[]` — nil, 0 fields.
const NIL_ID: DataConId = DataConId(13);

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    for (id, name, tag, arity, qualified) in [
        (VAL_ID, "Val", 0u32, 1u32, Some("Control.Monad.Freer.Val")),
        (E_ID, "E", 0, 2, Some("Control.Monad.Freer.E")),
        (UNION_ID, "Union", 0, 2, Some("Data.OpenUnion.Union")),
        (LEAF_ID, "Leaf", 0, 1, Some("Data.FTCQueue.Leaf")),
        (NODE_ID, "Node", 0, 2, Some("Data.FTCQueue.Node")),
        (RESULT_ID, "ThreadResult", 1, 1, None),
        (WRAP_ID, "SpawnWrap", 1, 2, None),
        (OUTER_ID, "WrapResult", 1, 1, None),
        (PAIR_ID, "Pair", 0, 2, None),
        (EVENT_ID, "Event", 0, 2, None),
        (WATCH_ID, "WatchMailbox", 0, 1, None),
        (CONS_ID, ":", 0, 2, Some("GHC.Types.:")),
        (NIL_ID, "[]", 0, 0, Some("GHC.Types.[]")),
        (RIGHT_ID, "Right", 0, 1, Some("Data.Either.Right")),
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

/// Build: `let shared = 999 in E (Union wrap_tag (SpawnWrap dummy bodyClosure))
/// (Leaf (\v -> Val (WrapResult (Pair shared v))))`, where `bodyClosure =
/// \_ -> E (Union body_tag (Pair shared body_lit)) (Leaf (\v -> Val
/// (ThreadResult v)))`.
///
/// `shared` is a free variable of BOTH `bodyClosure` (captured into the
/// closure that gets TENURED when this suspends) and the wrap's own
/// continuation `\v -> Val (WrapResult (Pair shared v))` (captured into a
/// SEPARATE, independently-rooted closure via ordinary lexical sharing) —
/// the exact shape `forkNode`'s `downMid`/`upMid` capture produces.
fn build_wrap_suspend_shared(wrap_tag: u64, dummy: i64, body_tag: u64, body_lit: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const SHARED_VAR: VarId = VarId(20);
    let shared_rhs = b.push(CoreFrame::Lit(Literal::LitInt(999)));

    // bodyClosure = `\_ -> E (Union body_tag (Pair shared body_lit))
    //                        (Leaf (\v -> Val (ThreadResult v)))`
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let shared_ref_inner = b.push(CoreFrame::Var(SHARED_VAR));
    let body_lit_node = b.push(CoreFrame::Lit(Literal::LitInt(body_lit)));
    let body_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![shared_ref_inner, body_lit_node],
    });
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, body_payload],
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

    // The scratch wrapper suspend, whose OWN continuation ALSO captures
    // `shared` — the load-bearing aliasing this repro is testing.
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
    let shared_ref_outer = b.push(CoreFrame::Var(SHARED_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![shared_ref_outer, outer_cont_v],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    b.push(CoreFrame::LetNonRec {
        binder: SHARED_VAR,
        rhs: shared_rhs,
        body: wrap_e,
    });
    b.build()
}

/// Like [`build_wrap_suspend_shared`], but `shared` is itself a CLOSURE (the
/// identity function) rather than plain data, and the wrap's own continuation
/// APPLIES its own captured copy of it after resume — the exact shape of the
/// real crash signature ("application of non-closure … tag 255"), not just a
/// stale data pointer sitting unread in a field.
fn build_wrap_suspend_shared_closure(
    wrap_tag: u64,
    dummy: i64,
    body_tag: u64,
    body_lit: i64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const SHARED_VAR: VarId = VarId(20);
    const SHARED_ARG: VarId = VarId(22);
    let shared_arg_v = b.push(CoreFrame::Var(SHARED_ARG));
    let shared_rhs = b.push(CoreFrame::Lam {
        binder: SHARED_ARG,
        body: shared_arg_v,
    }); // identity closure

    // bodyClosure = `\_ -> E (Union body_tag (Pair (shared body_lit) body_lit))
    //                        (Leaf (\v -> Val (ThreadResult v)))`
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let shared_ref_inner = b.push(CoreFrame::Var(SHARED_VAR));
    let body_lit_node = b.push(CoreFrame::Lit(Literal::LitInt(body_lit)));
    let shared_applied_inner = b.push(CoreFrame::App {
        fun: shared_ref_inner,
        arg: body_lit_node,
    });
    let body_lit_node2 = b.push(CoreFrame::Lit(Literal::LitInt(body_lit)));
    let body_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![shared_applied_inner, body_lit_node2],
    });
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, body_payload],
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

    // The scratch wrapper suspend: its OWN continuation applies its OWN
    // independently-captured copy of `shared` AFTER resume.
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
    let shared_ref_outer = b.push(CoreFrame::Var(SHARED_VAR));
    let apply_lit = b.push(CoreFrame::Lit(Literal::LitInt(999)));
    let shared_applied_outer = b.push(CoreFrame::App {
        fun: shared_ref_outer,
        arg: apply_lit,
    });
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![shared_applied_outer, outer_cont_v],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    b.push(CoreFrame::LetNonRec {
        binder: SHARED_VAR,
        rhs: shared_rhs,
        body: wrap_e,
    });
    b.build()
}

fn expect_int(v: &Value) -> i64 {
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        other => panic!("expected a LitInt, got {other:?}"),
    }
}

/// Unwrap `WrapResult (Pair shared v)` into `(shared, v)`.
fn expect_wrap_pair(v: &Value) -> (i64, i64) {
    match v {
        Value::Con(id, fields) if id.0 == OUTER_ID.0 && fields.len() == 1 => match &fields[0] {
            Value::Con(pid, pfields) if pid.0 == PAIR_ID.0 && pfields.len() == 2 => {
                (expect_int(&pfields[0]), expect_int(&pfields[1]))
            }
            other => panic!("expected Pair(shared, v) inside WrapResult, got {other:?}"),
        },
        other => panic!("expected WrapResult(Pair(..)), got {other:?}"),
    }
}

#[derive(Clone, Default)]
struct TestSink {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// Every tag this file constructs is >= the suspend threshold (0), so
/// everything suspends; a real dispatch call would mean the representation
/// under test silently stopped suspending.
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

/// THE REPRO: a collection forced between the suspend-time tenure of a
/// field-1 closure and the RESUME of the frame that sent it must not corrupt
/// a free variable the frame's own continuation shares with that closure.
#[test]
fn shared_free_variable_survives_forced_gc_between_tenure_and_resume() {
    let table = table();
    let mut session = fresh_session();

    let expr = build_wrap_suspend_shared(100, 1, 200, 11);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    // Sentinel-tenure fires here: `bodyClosure`'s transitive closure —
    // including `shared` — is evacuated into old-space and registered as a
    // persistent root. The wrap's OWN parked continuation independently
    // captured `shared` too, and that copy is NOT touched by this tenure.
    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // THE EXPERIMENT: force a real collection in the window between the
    // tenure above and the resume below, with nothing else running.
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (shared_back, _answer) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        shared_back, 999,
        "the wrap continuation's own (independently-captured) copy of `shared` \
         must survive a collection that ran after `bodyClosure`'s copy was tenured"
    );

    // The tenured body closure must also still be independently usable —
    // `run_forked` consumes the `RootCustody` `finalized_handle` minted above.
    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(77)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    let answer = match inner_outcome {
        ResidentOutcome::Completed { result, .. } => match result.into_value() {
            Value::Con(id, ref fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                expect_int(&fields[0])
            }
            other => panic!("expected ThreadResult(answer), got {other:?}"),
        },
        other => panic!("thread must complete, got {other:?}"),
    };
    assert_eq!(
        answer, 77,
        "the tenured closure's own result must survive delivery"
    );
}

/// THE REPRO, closure-applying variant: the wrap's own continuation APPLIES
/// its own independently-captured copy of a closure ALSO captured (and
/// tenured) by the field-1 body closure, after a forced collection — the
/// exact shape of the real crash signature (application of a non-closure,
/// tag 255), not just a stale pointer sitting unread in a data field.
#[test]
fn shared_closure_applies_correctly_after_forced_gc_between_tenure_and_resume() {
    let table = table();
    let mut session = fresh_session();

    let expr = build_wrap_suspend_shared_closure(100, 1, 200, 11);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // Multiple forced collections in the window between tenure and resume —
    // simulating "enough allocation for a collection to run" more than once.
    session.force_gc_for_test();
    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (applied_back, _answer) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        applied_back, 999,
        "applying the wrap continuation's own captured copy of the shared identity \
         closure must return its argument unchanged, not trap on a stale/forwarded pointer"
    );

    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(77)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    let answer = match inner_outcome {
        ResidentOutcome::Completed { result, .. } => match result.into_value() {
            Value::Con(id, ref fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                expect_int(&fields[0])
            }
            other => panic!("expected ThreadResult(answer), got {other:?}"),
        },
        other => panic!("thread must complete, got {other:?}"),
    };
    assert_eq!(
        answer, 77,
        "the tenured closure's own result must survive delivery"
    );
}

/// THE REPRO, real-driver-order variant: `tidepool-harness`'s `AsyncSpawnWith`
/// servicing (`selfharness/driver.rs`) mints the body handle, then runs the
/// CHILD thread (`run_forked`) FIRST — letting it allocate, and possibly
/// trigger a real collection — and only THEN resumes the spawner's own
/// parked frame. Both earlier repros in this file resumed the spawner
/// BEFORE running the child (the opposite order) and could not reproduce the
/// bug; this test matches the real order exactly, with a forced collection
/// in the child's own window, to test whether that ordering — not a bare
/// forced GC — is what the family's rooting gap actually needs.
#[test]
fn shared_closure_survives_when_child_thread_runs_before_spawner_resumes() {
    let table = table();
    let mut session = fresh_session();

    let expr = build_wrap_suspend_shared_closure(100, 1, 200, 11);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    // Sentinel-tenure fires here, exactly as the driver's AsyncSpawnWith arm
    // observes it (`finalized_handle` called on the just-suspended spawner's
    // own frame, BEFORE the spawner is resumed).
    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // REAL ORDER: run the child thread FIRST (mirrors driver.rs's
    // `run_forked` call preceding `resume(hole, tid_value)`), forcing a
    // collection while the child's own fragment is the active run and the
    // spawner's frame sits parked in the SAME registry.
    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    session.force_gc_for_test();

    // ONLY NOW resume the spawner — matching `resume(hole, tid_value)` in
    // driver.rs, which runs AFTER `run_forked` started the child.
    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (applied_back, _answer) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        applied_back, 999,
        "applying the wrap continuation's own captured copy of the shared identity \
         closure must return its argument unchanged after the child thread ran and a \
         collection fired, not trap on a stale/forwarded pointer"
    );

    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(77)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    let answer = match inner_outcome {
        ResidentOutcome::Completed { result, .. } => match result.into_value() {
            Value::Con(id, ref fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                expect_int(&fields[0])
            }
            other => panic!("expected ThreadResult(answer), got {other:?}"),
        },
        other => panic!("thread must complete, got {other:?}"),
    };
    assert_eq!(
        answer, 77,
        "the tenured closure's own result must survive delivery"
    );
}

/// A dispatcher that HANDLES tags 1/2 (returning fresh materialized Ints —
/// exercising `materialize_response_and_resume`'s real allocation path, NOT
/// a program literal) and refuses anything else — tags >= `ask_tag` (100 in
/// the test below) must never reach here; they suspend instead.
struct HandledThenSuspend;
impl DispatchEffect<TestSink> for HandledThenSuspend {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
        match tag {
            1 => Ok(Response::Complete(Value::Lit(Literal::LitInt(111)))),
            2 => Ok(Response::Complete(Value::Lit(Literal::LitInt(222)))),
            other => panic!("handler dispatched tag {other} — expected only 1/2"),
        }
    }
}

fn handled_session() -> ResidentSession<HandledThenSuspend, TestSink> {
    ResidentSession::unbootstrapped(
        HandledThenSuspend,
        100, // ask_tag: tags 1/2 are handled; tags >= 100 suspend.
        Vec::new(),
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
}

/// Build: `E(Union 1 dummy) (Leaf (\downMid -> E(Union 2 dummy) (Leaf (\upMid ->
///   E(Union wrap_tag (SpawnWrap dummy bodyClosure)) (Leaf (\v -> Val (WrapResult
///   (Pair downMid upMid))))))))`, where `bodyClosure = \_ -> E(Union body_tag
///   (Pair downMid upMid)) (Leaf (\v -> Val (ThreadResult v)))`.
///
/// `downMid`/`upMid` are NOT program literals (unlike `build_wrap_suspend_shared*`)
/// — they are the MATERIALIZED RESPONSES of two real, HANDLED effect
/// dispatches (`materialize_response_and_resume`'s allocation path), exactly
/// matching `mailboxNew >>= liftEither` binding `downMid`/`upMid` before
/// `forkNode` calls `async`. Both `bodyClosure` (tenured at the wrap's
/// suspend) and the wrap's own continuation (run on RESUME) capture BOTH as
/// free variables — matching the heap-trace finding of a 2-capture closure
/// whose capture[0] read tag 255.
fn build_wrap_suspend_handled_deps(wrap_tag: u64, dummy: i64, body_tag: u64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    const OUTER_CONT_VAR: VarId = VarId(2);

    // bodyClosure = `\_ -> E (Union body_tag (Pair downMid upMid))
    //                        (Leaf (\v -> Val (ThreadResult v)))`
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let down_ref_inner = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_inner = b.push(CoreFrame::Var(UP_VAR));
    let body_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_inner, up_ref_inner],
    });
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, body_payload],
    });
    // The body's OWN continuation ALSO independently captures downMid/upMid
    // (ignoring the resume answer) -- a SECOND, separate capture of the same
    // free variables, so this closure's own post-resume read is tested too,
    // not just the wrap's.
    let down_ref_body_cont = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_body_cont = b.push(CoreFrame::Var(UP_VAR));
    let cont_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_body_cont, up_ref_body_cont],
    });
    let cont_result = b.push(CoreFrame::Con {
        tag: RESULT_ID,
        fields: vec![cont_payload],
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

    // The wrap suspend: its own continuation ALSO captures downMid/upMid.
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
    let _outer_cont_v = b.push(CoreFrame::Var(OUTER_CONT_VAR));
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    // upMid = E(Union 2 dummy2) (Leaf (\upMid -> wrap_e))
    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_VAR,
        body: wrap_e,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    // downMid = E(Union 1 dummy1) (Leaf (\downMid -> up_e))
    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_VAR,
        body: up_e,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, handled-response variant: `downMid`/`upMid` are the
/// MATERIALIZED RESPONSES of two real handled dispatches (matching
/// `mailboxNew >>= liftEither` in `forkNode`), not program literals. Both the
/// tenured body closure and the wrap's own post-resume continuation capture
/// them independently. This is the shape the heap-validation trace on the
/// real GHC-driven repro caught directly: "INVALID closure: field 0 has
/// invalid tag: 255" on a 2-capture closure — matching a continuation that
/// captured exactly `downMid`/`upMid`.
#[test]
fn handled_dependencies_survive_gc_between_tenure_and_resume() {
    let table = table();
    let mut session = handled_session();

    let expr = build_wrap_suspend_handled_deps(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid (materialized handled \
         responses, not literals) must survive tenure + GC + resume intact"
    );

    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    match inner_outcome {
        ResidentOutcome::Completed { result, .. } => match result.into_value() {
            Value::Con(id, ref fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                match &fields[0] {
                    Value::Con(pid, pfields) if pid.0 == PAIR_ID.0 && pfields.len() == 2 => {
                        assert_eq!(
                            (expect_int(&pfields[0]), expect_int(&pfields[1])),
                            (111, 222),
                            "the tenured body closure's own captured downMid/upMid must \
                             also survive tenure + GC + resume + run_forked intact"
                        );
                    }
                    other => panic!("expected Pair(downMid, upMid), got {other:?}"),
                }
            }
            other => panic!("expected ThreadResult(answer), got {other:?}"),
        },
        other => panic!("thread must complete, got {other:?}"),
    }
}

/// Build a wrap suspend whose body closure COMPLETES SYNCHRONOUSLY inside
/// `run_forked` (dispatches one HANDLED effect, then `Val`s) — never parking
/// a second registry frame — matching `forkNode`'s real body shape
/// (`sendUp (uplink ctx) 777; pure 0`, where `sendUp` is a handled
/// `RepoEvent`, not a suspend). Every earlier repro in this file forced the
/// child to SUSPEND (registering ITS OWN stowed root); this is the first to
/// test the synchronously-completing shape while the spawner's frame sits
/// parked in the registry.
fn build_wrap_suspend_sync_child(wrap_tag: u64, dummy: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CHILD_CONT_VAR: VarId = VarId(3);
    const OUTER_CONT_VAR: VarId = VarId(2);

    // bodyClosure = `\_ -> E (Union 1 dummy) (Leaf (\_r -> Val (ThreadResult
    //                        (Pair downMid upMid))))` — dispatches ONE
    // handled effect (allocating a real response), then completes.
    let handled_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let handled_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![handled_tag_lit, handled_req],
    });
    let down_ref_body_cont = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_body_cont = b.push(CoreFrame::Var(UP_VAR));
    let cont_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_body_cont, up_ref_body_cont],
    });
    let cont_result = b.push(CoreFrame::Con {
        tag: RESULT_ID,
        fields: vec![cont_payload],
    });
    let cont_val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![cont_result],
    });
    let cont_lam = b.push(CoreFrame::Lam {
        binder: CHILD_CONT_VAR,
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

    // The wrap suspend: its own continuation ALSO captures downMid/upMid.
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
    let _outer_cont_v = b.push(CoreFrame::Var(OUTER_CONT_VAR));
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    // upMid = E(Union 2 dummy2) (Leaf (\upMid -> wrap_e))
    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_VAR,
        body: wrap_e,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    // downMid = E(Union 1 dummy1) (Leaf (\downMid -> up_e))
    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_VAR,
        body: up_e,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, synchronously-completing-child variant: `run_forked`'s child
/// dispatches one handled effect and COMPLETES within that same call — never
/// registering its own parked frame — while the spawner's frame sits parked
/// in the registry. Matches `forkNode`'s real body (`sendUp` is handled, not
/// a suspend) more closely than every earlier variant in this file, all of
/// which forced the child to suspend.
#[test]
fn handled_dependencies_survive_when_child_completes_synchronously() {
    let table = table();
    let mut session = handled_session();

    let expr = build_wrap_suspend_sync_child(100, 1);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // The child dispatches a handled effect and completes SYNCHRONOUSLY here
    // — no new parked frame, but real allocation (response materialization)
    // happens while the spawner's frame is still parked in the registry.
    let child_result = session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"));
    let (child_down, child_up) = match child_result {
        ResidentOutcome::Completed { result, .. } => match result.into_value() {
            Value::Con(id, ref fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                match &fields[0] {
                    Value::Con(pid, pfields) if pid.0 == PAIR_ID.0 && pfields.len() == 2 => {
                        (expect_int(&pfields[0]), expect_int(&pfields[1]))
                    }
                    other => panic!("expected Pair(downMid, upMid), got {other:?}"),
                }
            }
            other => panic!("expected ThreadResult(answer), got {other:?}"),
        },
        other => panic!("expected the child to complete synchronously (no suspend), got {other:?}"),
    };
    assert_eq!(
        (child_down, child_up),
        (111, 222),
        "the child's own captured downMid/upMid must survive its synchronous \
         completion while the spawner's frame sits parked"
    );

    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid must survive tenure + \
         a synchronously-completing child + GC + resume intact"
    );
}

/// Build the SAME shape as [`build_wrap_suspend_shared`], except `shared`'s
/// RHS is an UNFORCED REDEX (`(\y -> Pair y y) 999`) rather than an
/// already-WHNF `Lit` — so `shared` is compiled as a genuine lazy THUNK
/// object, not a plain heap `Lit`. Both `bodyClosure` (tenured) and the
/// wrap's own continuation capture this SAME thunk and FORCE it (read its
/// value) only AFTER the suspend/tenure — testing whether a shared
/// UNEVALUATED thunk's own captures survive tenure + GC + resume, a case
/// none of this file's other repros (which only ever shared already-WHNF
/// Lits or Lams) exercise.
fn build_wrap_suspend_shared_thunk(
    wrap_tag: u64,
    dummy: i64,
    body_tag: u64,
    body_lit: i64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const SHARED_VAR: VarId = VarId(20);
    const REDEX_ARG: VarId = VarId(23);

    // shared = (\y -> Pair y y) 999 -- an unforced redex (a real thunk).
    let redex_arg_v = b.push(CoreFrame::Var(REDEX_ARG));
    let redex_pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![redex_arg_v, redex_arg_v],
    });
    let redex_lam = b.push(CoreFrame::Lam {
        binder: REDEX_ARG,
        body: redex_pair,
    });
    let redex_lit = b.push(CoreFrame::Lit(Literal::LitInt(999)));
    let shared_rhs = b.push(CoreFrame::App {
        fun: redex_lam,
        arg: redex_lit,
    });

    // bodyClosure = `\_ -> E (Union body_tag (fst shared)) (Leaf (\v -> Val (ThreadResult v)))`
    // "fst shared" forces the thunk: pattern-match its first field via a
    // trivial Con-field read (there is no case-of-1-alt builder here, so we
    // read field 0 directly through a Con with a single "unwrap" field —
    // simplest is to just embed `shared` itself as the payload, which forces
    // it during heap bridging of the suspend request).
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let shared_ref_inner = b.push(CoreFrame::Var(SHARED_VAR));
    let body_lit_node = b.push(CoreFrame::Lit(Literal::LitInt(body_lit)));
    let body_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![shared_ref_inner, body_lit_node],
    });
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, body_payload],
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

    // The wrap's own continuation ALSO captures `shared` and forces it (by
    // embedding it directly, forcing at heap-bridge time) ONLY on resume.
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
    let shared_ref_outer = b.push(CoreFrame::Var(SHARED_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![shared_ref_outer, outer_cont_v],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    b.push(CoreFrame::LetNonRec {
        binder: SHARED_VAR,
        rhs: shared_rhs,
        body: wrap_e,
    });
    b.build()
}

/// THE REPRO, shared-THUNK variant: `shared` is a genuine lazy, UNFORCED
/// thunk (an unforced redex) at suspend time — not an already-WHNF `Lit`/
/// `Lam` like every other repro in this file — captured by both the tenured
/// body closure and the wrap's own continuation, forced only after tenure +
/// GC + resume.
#[test]
fn shared_unforced_thunk_survives_forced_gc_between_tenure_and_resume() {
    let table = table();
    let mut session = fresh_session();

    let expr = build_wrap_suspend_shared_thunk(100, 1, 200, 11);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    session.force_gc_for_test();
    session.force_gc_for_test();

    // Resuming forces the wrap continuation, which forces `shared` for the
    // FIRST time here — after tenure and two collections.
    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (shared_pair, _answer) = match outcome {
        ResidentOutcome::Completed { result, .. } => {
            let v = result.into_value();
            match &v {
                Value::Con(id, fields) if id.0 == OUTER_ID.0 && fields.len() == 1 => {
                    match &fields[0] {
                        Value::Con(pid, pfields) if pid.0 == PAIR_ID.0 && pfields.len() == 2 => {
                            (pfields[0].clone(), pfields[1].clone())
                        }
                        other => panic!("expected Pair(shared, v), got {other:?}"),
                    }
                }
                other => panic!("expected WrapResult(Pair(..)), got {other:?}"),
            }
        }
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    match &shared_pair {
        Value::Con(pid, pfields) if pid.0 == PAIR_ID.0 && pfields.len() == 2 => {
            assert_eq!(
                (expect_int(&pfields[0]), expect_int(&pfields[1])),
                (999, 999),
                "forcing the wrap continuation's own captured (shared, unforced-at-tenure) \
                 thunk after tenure + GC + resume must yield the redex's real value, not a \
                 stale/forwarded read"
            );
        }
        other => panic!("expected Pair(999, 999) from forcing the shared thunk, got {other:?}"),
    }

    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(77)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    match inner_outcome {
        ResidentOutcome::Completed { result, .. } => {
            let v = result.into_value();
            match &v {
                Value::Con(id, fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                    assert_eq!(expect_int(&fields[0]), 77);
                }
                other => panic!("expected ThreadResult(answer), got {other:?}"),
            }
        }
        other => panic!("thread must complete, got {other:?}"),
    }
}

/// Build the EXACT shape that isolated the real bug via source-level
/// bisection (`tidepool-harness/tests/fixtures/MinimalEventCaptureHarness.hs`
/// — `MinimalAsyncHarness.hs`, capturing only two handled-effect Ints,
/// passed; adding ONE raw `Event Value` (`mailbox downMid`, no `fmap`) to
/// the captured environment reproduced the crash). `bodyClosure`'s
/// environment captures `downMid`/`upMid` (materialized handled responses)
/// AND an `Event`-shaped Con (`Event [WatchMailbox downMid] (\r -> downMid)`
/// — a LIST field plus a CLOSURE field, matching
/// `tidepool-mcp/src/effect_defs.rs`'s generated `mailbox`) — but the body
/// itself never references or forces the Event, exactly like
/// `asyncBody _upMid _inbox = pure 0` ignoring both arguments.
fn build_wrap_suspend_with_event_capture(wrap_tag: u64, dummy: i64, body_tag: u64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    const OUTER_CONT_VAR: VarId = VarId(2);
    const PROJECT_ARG: VarId = VarId(32);

    // eventCon = Event [WatchMailbox downMid] (\r -> downMid)
    let down_ref_for_watch = b.push(CoreFrame::Var(DOWN_VAR));
    let watch_con = b.push(CoreFrame::Con {
        tag: WATCH_ID,
        fields: vec![down_ref_for_watch],
    });
    let nil = b.push(CoreFrame::Con {
        tag: NIL_ID,
        fields: vec![],
    });
    let watch_list = b.push(CoreFrame::Con {
        tag: CONS_ID,
        fields: vec![watch_con, nil],
    });
    let down_ref_for_project = b.push(CoreFrame::Var(DOWN_VAR));
    let project_lam = b.push(CoreFrame::Lam {
        binder: PROJECT_ARG,
        body: down_ref_for_project,
    });
    let event_con = b.push(CoreFrame::Con {
        tag: EVENT_ID,
        fields: vec![watch_list, project_lam],
    });

    // bodyClosure = `\_ -> let _unused = eventCon in
    //                      E (Union body_tag upMid) (Leaf (\v -> Val (ThreadResult v)))`
    // -- `event_con` is captured (a free variable of this Lam's body via the
    // LetNonRec below) but never read past construction, matching
    // `asyncBody`'s dead `_inbox` argument.
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let up_ref_inner = b.push(CoreFrame::Var(UP_VAR));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, up_ref_inner],
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
    const EVENT_LET_VAR: VarId = VarId(33);
    let event_let_ref = b.push(CoreFrame::Var(EVENT_LET_VAR));
    // Force the reference to exist syntactically (so `event_con` is a real
    // free variable of the closure) without altering the body's result: wrap
    // `body_e` unchanged, but bind `event_con` around it via LetNonRec.
    let _ = event_let_ref; // silence "unused" — the Var node itself is unused on purpose
    let body_with_let = b.push(CoreFrame::LetNonRec {
        binder: EVENT_LET_VAR,
        rhs: event_con,
        body: body_e,
    });
    let body_closure = b.push(CoreFrame::Lam {
        binder: ARG_VAR,
        body: body_with_let,
    });

    // The wrap suspend: its own continuation captures downMid/upMid (as the
    // baseline handled-deps test does) but NOT the Event.
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
    let _outer_cont_v = b.push(CoreFrame::Var(OUTER_CONT_VAR));
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    // upMid = E(Union 2 dummy2) (Leaf (\upMid -> wrap_e))
    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_VAR,
        body: wrap_e,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    // downMid = E(Union 1 dummy1) (Leaf (\downMid -> up_e))
    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_VAR,
        body: up_e,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, Event-capture variant: matches the source-level bisection
/// exactly — `bodyClosure` captures downMid/upMid AND an `Event`-shaped Con
/// (list field + closure field) it never reads, tenured at the wrap's
/// suspend. Neither the wrap's own resume NOR the child's completion+result
/// read (mirrored below) should corrupt anything if the rooting discipline
/// is complete for this shape.
#[test]
fn event_shaped_capture_survives_tenure_and_resume() {
    let table = table();
    let mut session = handled_session();

    let expr = build_wrap_suspend_with_event_capture(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid must survive tenure of a \
         SIBLING closure that also captured an Event-shaped (list+closure) Con"
    );

    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(222)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    match inner_outcome {
        ResidentOutcome::Completed { result, .. } => {
            let v = result.into_value();
            match &v {
                Value::Con(id, fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                    assert_eq!(expect_int(&fields[0]), 222, "echoed resume answer via ThreadResult");
                }
                other => panic!("expected ThreadResult(answer), got {other:?}"),
            }
        }
        other => panic!(
            "thread must complete cleanly (matching AsyncResultWith's real resume point), got {other:?}"
        ),
    }
}

fn tiny_handled_session() -> ResidentSession<HandledThenSuspend, TestSink> {
    ResidentSession::unbootstrapped(
        HandledThenSuspend,
        100,
        Vec::new(),
        TestSink::default(),
        Vec::new(),
        // Deliberately tiny: forces REAL, in-flight `gc_trigger`s during
        // ordinary allocation (Event/Watch/list/closure construction) —
        // unlike `force_gc_for_test`, which always runs with ZERO live JIT
        // stack frames and so can never exercise a stack-map coverage gap
        // for a value still live in a register/stack slot (not yet in any
        // heap-captured environment) at the moment of collection.
        512,
        None,
    )
}

/// Same shape as [`event_shaped_capture_survives_tenure_and_resume`], but
/// with a TINY nursery so ordinary allocation triggers REAL, in-flight
/// collections (stack-map-tracked JIT frames included in the root set) —
/// instead of `force_gc_for_test`'s synthetic, zero-stack-frame collection.
#[test]
fn event_shaped_capture_survives_real_inflight_gc_with_tiny_nursery() {
    let table = table();
    let mut session = tiny_handled_session();

    let expr = build_wrap_suspend_with_event_capture(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .unwrap_or_else(|e| panic!("wrap suspend run failed: {e}"));
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));

    let gc_count_after = session.heap_stats().map(|s| s.gc_count).unwrap_or(0);
    assert!(
        gc_count_after > 0,
        "sanity: the tiny (512-byte) nursery must have triggered at least one REAL, \
         in-flight collection somewhere in this run (gc_count={gc_count_after}) -- \
         otherwise this test isn't exercising what it claims to"
    );

    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid must survive tenure of a \
         SIBLING closure that also captured an Event-shaped Con, under REAL in-flight GC"
    );

    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(222)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    match inner_outcome {
        ResidentOutcome::Completed { result, .. } => {
            let v = result.into_value();
            match &v {
                Value::Con(id, fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                    assert_eq!(expect_int(&fields[0]), 222);
                }
                other => panic!("expected ThreadResult(answer), got {other:?}"),
            }
        }
        other => panic!("thread must complete, got {other:?}"),
    }
}

/// `Right x` — 1-field Con, mirroring `Either EventError Int`'s success arm
/// (`mailboxNew :: M (Either EventError Int)`, unwrapped by `liftEither`'s
/// own `case` match — a layer none of this file's other repros model; they
/// all bind the dispatch response directly, with no intermediate `Either`
/// unwrap).
const RIGHT_ID: DataConId = DataConId(14);

/// A dispatcher that HANDLES tags 1/2 by returning `Right 111`/`Right 222`
/// (an `Either`-shaped Con), matching `mailboxNew`'s real response shape —
/// the caller must `case`-unwrap it, exactly like `liftEither`.
struct HandledEitherThenSuspend;
impl DispatchEffect<TestSink> for HandledEitherThenSuspend {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
        match tag {
            1 => Ok(Response::Complete(Value::Con(
                RIGHT_ID,
                vec![Value::Lit(Literal::LitInt(111))],
            ))),
            2 => Ok(Response::Complete(Value::Con(
                RIGHT_ID,
                vec![Value::Lit(Literal::LitInt(222))],
            ))),
            other => panic!("handler dispatched tag {other} — expected only 1/2"),
        }
    }
}

fn handled_either_session() -> ResidentSession<HandledEitherThenSuspend, TestSink> {
    ResidentSession::unbootstrapped(
        HandledEitherThenSuspend,
        100,
        Vec::new(),
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
}

/// Like [`build_wrap_suspend_with_event_capture`], but `downMid`/`upMid` are
/// bound by CASE-UNWRAPPING an `Either`-shaped (`Right x`) dispatch
/// response — mirroring `mailboxNew >>= liftEither` exactly, rather than
/// binding a dispatch response directly as every other repro in this file
/// does. `RIGHT_ID` also gives `tenure_finalized_payload`'s transitive
/// closure walk one more constructor SHAPE to evacuate.
fn build_wrap_suspend_event_via_either_unwrap(
    wrap_tag: u64,
    dummy: i64,
    body_tag: u64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    const OUTER_CONT_VAR: VarId = VarId(2);
    const PROJECT_ARG: VarId = VarId(32);
    const EVENT_LET_VAR: VarId = VarId(33);
    const DOWN_RESP_SCRUT: VarId = VarId(34);
    const UP_RESP_SCRUT: VarId = VarId(35);

    // eventCon = Event [WatchMailbox downMid] (\r -> downMid)
    let down_ref_for_watch = b.push(CoreFrame::Var(DOWN_VAR));
    let watch_con = b.push(CoreFrame::Con {
        tag: WATCH_ID,
        fields: vec![down_ref_for_watch],
    });
    let nil = b.push(CoreFrame::Con {
        tag: NIL_ID,
        fields: vec![],
    });
    let watch_list = b.push(CoreFrame::Con {
        tag: CONS_ID,
        fields: vec![watch_con, nil],
    });
    let down_ref_for_project = b.push(CoreFrame::Var(DOWN_VAR));
    let project_lam = b.push(CoreFrame::Lam {
        binder: PROJECT_ARG,
        body: down_ref_for_project,
    });
    let event_con = b.push(CoreFrame::Con {
        tag: EVENT_ID,
        fields: vec![watch_list, project_lam],
    });

    // bodyClosure = `\_ -> let _unused = eventCon in
    //                      E (Union body_tag upMid) (Leaf (\v -> Val (ThreadResult v)))`
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let up_ref_inner = b.push(CoreFrame::Var(UP_VAR));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, up_ref_inner],
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
    let body_with_let = b.push(CoreFrame::LetNonRec {
        binder: EVENT_LET_VAR,
        rhs: event_con,
        body: body_e,
    });
    let body_closure = b.push(CoreFrame::Lam {
        binder: ARG_VAR,
        body: body_with_let,
    });

    // The wrap suspend: its own continuation captures downMid/upMid.
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
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    // upMid: dispatch tag 2 -> Right x -> case-unwrap to bind UP_VAR -> wrap_e
    let up_scrut_var = b.push(CoreFrame::Var(UP_RESP_SCRUT));
    let up_case = b.push(CoreFrame::Case {
        scrutinee: up_scrut_var,
        binder: VarId(36),
        alts: vec![Alt {
            con: AltCon::DataAlt(RIGHT_ID),
            binders: vec![UP_VAR],
            body: wrap_e,
        }],
    });
    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_RESP_SCRUT,
        body: up_case,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    // downMid: dispatch tag 1 -> Right x -> case-unwrap to bind DOWN_VAR -> up_e
    let down_scrut_var = b.push(CoreFrame::Var(DOWN_RESP_SCRUT));
    let down_case = b.push(CoreFrame::Case {
        scrutinee: down_scrut_var,
        binder: VarId(37),
        alts: vec![Alt {
            con: AltCon::DataAlt(RIGHT_ID),
            binders: vec![DOWN_VAR],
            body: up_e,
        }],
    });
    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_RESP_SCRUT,
        body: down_case,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, Either-unwrap variant: `downMid`/`upMid` are bound by
/// CASE-UNWRAPPING an `Either`-shaped dispatch response (`Right x`),
/// mirroring `mailboxNew >>= liftEither` exactly. Every earlier repro in
/// this file bound a dispatch response directly.
#[test]
fn event_capture_survives_via_either_unwrap_binding() {
    let table = table();
    let mut session = handled_either_session();

    let expr = build_wrap_suspend_event_via_either_unwrap(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .unwrap_or_else(|e| panic!("wrap suspend run failed: {e}"));
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "downMid/upMid bound via an Either-unwrap case match must survive tenure + GC + \
         resume, with a sibling closure also capturing an Event-shaped Con"
    );

    let _ = inner_hole;
}

/// Like [`build_wrap_suspend_with_event_capture`], but `bodyClosure` captures
/// ONLY a bare list (`[WatchMailbox downMid]`, i.e. `Cons(Con[WATCH_ID,
/// downMid], Nil)`) — no `Event` wrapper Con, no closure field at all.
/// Isolates whether the LIST alone (not the closure `Event.eventProject`
/// carries) is what the earlier `Event`-shaped repro's rooting discipline
/// mishandles, matching `MinimalWatchListHarness.hs`'s source-level
/// bisection (which ALSO reproduces with the closure field dropped).
fn build_wrap_suspend_with_bare_list_capture(wrap_tag: u64, dummy: i64, body_tag: u64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    const OUTER_CONT_VAR: VarId = VarId(2);
    const LIST_LET_VAR: VarId = VarId(33);

    // watchList = [WatchMailbox downMid] = Cons(WatchMailbox downMid, Nil)
    let down_ref_for_watch = b.push(CoreFrame::Var(DOWN_VAR));
    let watch_con = b.push(CoreFrame::Con {
        tag: WATCH_ID,
        fields: vec![down_ref_for_watch],
    });
    let nil = b.push(CoreFrame::Con {
        tag: NIL_ID,
        fields: vec![],
    });
    let watch_list = b.push(CoreFrame::Con {
        tag: CONS_ID,
        fields: vec![watch_con, nil],
    });

    // bodyClosure = `\_ -> let _unused = watchList in
    //                      E (Union body_tag upMid) (Leaf (\v -> Val (ThreadResult v)))`
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let up_ref_inner = b.push(CoreFrame::Var(UP_VAR));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, up_ref_inner],
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
    let body_with_let = b.push(CoreFrame::LetNonRec {
        binder: LIST_LET_VAR,
        rhs: watch_list,
        body: body_e,
    });
    let body_closure = b.push(CoreFrame::Lam {
        binder: ARG_VAR,
        body: body_with_let,
    });

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
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_VAR,
        body: wrap_e,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_VAR,
        body: up_e,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, bare-list variant: `bodyClosure` captures ONLY a list
/// (`[WatchMailbox downMid]`), no closure field, matching the source-level
/// bisection's finding that the closure field is NOT required.
#[test]
fn bare_list_capture_survives_tenure_and_resume() {
    let table = table();
    let mut session = handled_session();

    let expr = build_wrap_suspend_with_bare_list_capture(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .unwrap_or_else(|e| panic!("wrap suspend run failed: {e}"));
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    let _inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid must survive tenure of a \
         SIBLING closure that captured only a bare list"
    );
}

/// THE FIX: every earlier repro in this file bound its "risky" captured
/// structure (`shared`, the `Event` Con, the bare list) via `LetNonRec`
/// with a body that never referenced the binder — and
/// `tidepool-codegen/src/emit/expr.rs`'s `LetNonRec` compilation has an
/// explicit DCE-skip for EXACTLY that shape ("Dead code elimination: skip
/// RHS if binder is unused in body"): the RHS is NEVER EVALUATED, NEVER
/// ALLOCATED. Every earlier test in this file was therefore silently
/// testing nothing — the list/Event/thunk it claimed to capture was never
/// built at all.
///
/// `MinimalWatchListHarness.hs`'s real shape is NOT a `let`: `asyncBody
/// upMid [WatchMailbox downMid]` passes the list as a genuine FUNCTION
/// ARGUMENT to `asyncBody`, which ignores it — the compiler cannot DCE an
/// argument to an opaque (`Var`-referenced) callee, so the list is
/// genuinely allocated (as a thunk, per lazy calling convention) and
/// captured, then NEVER forced by anything for the rest of the program.
///
/// This builds that exact shape: `watchList` is bound via `LetNonRec` at
/// the OUTER scope (alongside `downMid`/`upMid`), so it is a CAPTURED FREE
/// VARIABLE of `bodyClosure` — built ONCE, BEFORE the wrap suspends and
/// tenures — referenced only via `Var`, never constructed inline inside
/// `bodyClosure`'s own body (which would defer its allocation until
/// `bodyClosure` is APPLIED, i.e. well after resume — not what
/// `async (asyncBody upMid (mailbox downMid))` does: the argument
/// expression is evaluated at the call site, captured, and handed to a
/// closure that ignores it). `bodyInner` ignores its `watchList` argument,
/// exactly like `asyncBody`. The list is never embedded in anything the
/// test reads back.
fn build_wrap_suspend_with_undce_able_list_capture(
    wrap_tag: u64,
    dummy: i64,
    body_tag: u64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    const OUTER_CONT_VAR: VarId = VarId(2);
    const BODY_INNER_VAR: VarId = VarId(40);
    const IGNORED_WATCHES: VarId = VarId(41);
    const WATCH_LIST_VAR: VarId = VarId(42);

    // bodyInner = \ignoredWatches -> E (Union body_tag upMid)
    //                                   (Leaf (\v -> Val (ThreadResult v)))
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let up_ref_inner = b.push(CoreFrame::Var(UP_VAR));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, up_ref_inner],
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
    let body_inner_lam = b.push(CoreFrame::Lam {
        binder: IGNORED_WATCHES,
        body: body_e,
    });

    // bodyClosure = \_ -> App(Var(bodyInner), Var(watchList)) -- both the
    // callee AND the argument are opaque Var references into the OUTER
    // scope, so bodyClosure's own captures include watchList itself (the
    // already-built Con graph), not the code to build it.
    let body_inner_ref = b.push(CoreFrame::Var(BODY_INNER_VAR));
    let watch_list_ref = b.push(CoreFrame::Var(WATCH_LIST_VAR));
    let app_expr = b.push(CoreFrame::App {
        fun: body_inner_ref,
        arg: watch_list_ref,
    });
    let body_closure = b.push(CoreFrame::Lam {
        binder: ARG_VAR,
        body: app_expr,
    });

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
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    // bodyClosure REFERENCES BODY_INNER_VAR (via app_expr), so this
    // LetNonRec is NOT DCE'd -- bodyInner is a trivial (already-WHNF) Lam,
    // evaluated eagerly.
    let with_body_inner = b.push(CoreFrame::LetNonRec {
        binder: BODY_INNER_VAR,
        rhs: body_inner_lam,
        body: wrap_e,
    });

    // watchList = [WatchMailbox downMid] = Cons(WatchMailbox downMid, Nil),
    // bound HERE (outer scope, alongside downMid/upMid) so it is a
    // CAPTURED FREE VARIABLE of bodyClosure, built ONCE before the wrap
    // suspends -- not reconstructed when bodyClosure is later applied.
    // Referenced (via watch_list_ref inside app_expr, transitively through
    // with_body_inner/wrap_e/body_closure), so NOT DCE'd; `Con` is
    // `is_trivial_field`, so this evaluates eagerly (a real heap Con, not a
    // thunk) -- still the exact captured-Con-graph shape under test.
    let down_ref_for_watch = b.push(CoreFrame::Var(DOWN_VAR));
    let watch_con = b.push(CoreFrame::Con {
        tag: WATCH_ID,
        fields: vec![down_ref_for_watch],
    });
    let nil = b.push(CoreFrame::Con {
        tag: NIL_ID,
        fields: vec![],
    });
    let watch_list = b.push(CoreFrame::Con {
        tag: CONS_ID,
        fields: vec![watch_con, nil],
    });
    let with_watch_list = b.push(CoreFrame::LetNonRec {
        binder: WATCH_LIST_VAR,
        rhs: watch_list,
        body: with_body_inner,
    });

    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_VAR,
        body: with_watch_list,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_VAR,
        body: up_e,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, non-DCE'able list variant — see
/// `build_wrap_suspend_with_undce_able_list_capture`'s doc for why every
/// earlier repro in this file was silently testing nothing.
#[test]
fn undceable_list_capture_survives_tenure_and_resume() {
    let table = table();
    let mut session = handled_session();

    let expr = build_wrap_suspend_with_undce_able_list_capture(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .unwrap_or_else(|e| panic!("wrap suspend run failed: {e}"));
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // bodyInner ignores watchList but still suspends on body_tag (its own
    // body is `E (Union body_tag upMid) (...)`) -- the risky step is the
    // TENURE of bodyClosure, whose transitive graph includes the
    // genuinely-allocated, never-forced watchList thunk, alongside upMid.
    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    // Left parked, never resumed -- matching AsyncDoneWith's real discipline
    // (a thread's completion frame stays parked until its realm closes).
    let _ = &inner_hole;

    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid must survive tenure of a \
         sibling closure whose transitive graph includes a GENUINELY ALLOCATED, \
         never-forced list thunk (matching MinimalWatchListHarness.hs exactly)"
    );
}

/// Repro #12 — the MOST faithful shape yet, per the second-opinion
/// investigation's exact suggestion: `asyncSpawn x = send (AsyncSpawnWith 0
/// (\_ -> x))` captures `x` — a PRE-EXISTING THUNK bound OUTSIDE the field-1
/// closure, at the `async (...)` call site — as bodyClosure's ONLY free
/// variable, not code that re-evaluates an application each time
/// bodyClosure is invoked (repro #11's shape). `x`'s own captured
/// environment (built when `x` itself was thunked, non-trivially, per
/// `emit_thunk`) is what contains `bodyInner` and `watchList`. Tenuring
/// `bodyClosure` must therefore evacuate `x`'s ENTIRE transitive closure
/// too — `OldSpace::tenure`'s own doc promises exactly this ("Evacuates
/// ptr's entire transitive closure at tenure time").
fn build_wrap_suspend_with_thunked_app_capture(
    wrap_tag: u64,
    dummy: i64,
    body_tag: u64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();

    const DOWN_VAR: VarId = VarId(30);
    const UP_VAR: VarId = VarId(31);
    const ARG_VAR: VarId = VarId(1);
    const CONT_VAR: VarId = VarId(0);
    const OUTER_CONT_VAR: VarId = VarId(2);
    const BODY_INNER_VAR: VarId = VarId(40);
    const IGNORED_WATCHES: VarId = VarId(41);
    const WATCH_LIST_VAR: VarId = VarId(42);
    const X_VAR: VarId = VarId(43);

    // bodyInner = \ignoredWatches -> E (Union body_tag upMid)
    //                                   (Leaf (\v -> Val (ThreadResult v)))
    let body_tag_lit = b.push(CoreFrame::Lit(Literal::LitWord(body_tag)));
    let up_ref_inner = b.push(CoreFrame::Var(UP_VAR));
    let body_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![body_tag_lit, up_ref_inner],
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
    let body_inner_lam = b.push(CoreFrame::Lam {
        binder: IGNORED_WATCHES,
        body: body_e,
    });

    // bodyClosure = \_ -> Var(X) -- captures ONLY the pre-existing thunk X,
    // matching `\_ -> x` exactly (asyncSpawn's real shape).
    let x_ref = b.push(CoreFrame::Var(X_VAR));
    let body_closure = b.push(CoreFrame::Lam {
        binder: ARG_VAR,
        body: x_ref,
    });

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
    let down_ref_outer = b.push(CoreFrame::Var(DOWN_VAR));
    let up_ref_outer = b.push(CoreFrame::Var(UP_VAR));
    let outer_payload = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![down_ref_outer, up_ref_outer],
    });
    let outer_result = b.push(CoreFrame::Con {
        tag: OUTER_ID,
        fields: vec![outer_payload],
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
    let wrap_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![wrap_union, outer_leaf],
    });

    // X = App(Var(bodyInner), Var(watchList)) -- NON-TRIVIAL RHS (App), so
    // this becomes a genuine runtime THUNK (emit_thunk). NESTING ORDER
    // MATTERS: X's RHS references BODY_INNER_VAR/WATCH_LIST_VAR, so X's own
    // LetNonRec must be the INNERMOST of the three -- nested inside both of
    // theirs, not the other way around (a LetNonRec's RHS is scoped by its
    // ANCESTORS, never by its own `body` sibling).
    let body_inner_ref = b.push(CoreFrame::Var(BODY_INNER_VAR));
    let watch_list_ref = b.push(CoreFrame::Var(WATCH_LIST_VAR));
    let x_rhs = b.push(CoreFrame::App {
        fun: body_inner_ref,
        arg: watch_list_ref,
    });
    let with_x = b.push(CoreFrame::LetNonRec {
        binder: X_VAR,
        rhs: x_rhs,
        body: wrap_e,
    });

    // bodyInner: trivial (Lam), NOT DCE'd (referenced by X's RHS above).
    let with_body_inner = b.push(CoreFrame::LetNonRec {
        binder: BODY_INNER_VAR,
        rhs: body_inner_lam,
        body: with_x,
    });

    // watchList = [WatchMailbox downMid]: trivial (Con of Vars), NOT DCE'd
    // (referenced by X's RHS above).
    let down_ref_for_watch = b.push(CoreFrame::Var(DOWN_VAR));
    let watch_con = b.push(CoreFrame::Con {
        tag: WATCH_ID,
        fields: vec![down_ref_for_watch],
    });
    let nil = b.push(CoreFrame::Con {
        tag: NIL_ID,
        fields: vec![],
    });
    let watch_list = b.push(CoreFrame::Con {
        tag: CONS_ID,
        fields: vec![watch_con, nil],
    });
    let with_watch_list = b.push(CoreFrame::LetNonRec {
        binder: WATCH_LIST_VAR,
        rhs: watch_list,
        body: with_body_inner,
    });

    let up_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(2)));
    let up_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let up_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![up_dummy_tag, up_dummy_req],
    });
    let up_lam = b.push(CoreFrame::Lam {
        binder: UP_VAR,
        body: with_watch_list,
    });
    let up_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![up_lam],
    });
    let up_e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![up_union, up_leaf],
    });

    let down_dummy_tag = b.push(CoreFrame::Lit(Literal::LitWord(1)));
    let down_dummy_req = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let down_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![down_dummy_tag, down_dummy_req],
    });
    let down_lam = b.push(CoreFrame::Lam {
        binder: DOWN_VAR,
        body: up_e,
    });
    let down_leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![down_lam],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![down_union, down_leaf],
    });

    b.build()
}

/// THE REPRO, thunked-application-capture variant — see
/// `build_wrap_suspend_with_thunked_app_capture`'s doc.
///
/// Unlike repro #11 (`undceable_list_capture_survives_tenure_and_resume`,
/// which is ALSO a genuine capture but re-evaluates the application every
/// time `bodyClosure` is invoked), `bodyClosure` here captures ONLY a
/// pre-built thunk `X`, matching `asyncSpawn`'s real `\_ -> x` shape
/// exactly. `X` DOES get forced — `run_forked`'s own step-decoding must
/// force whatever `bodyClosure` returns to WHNF to classify it as `Val`/
/// `E` — so this exercises `tenure_finalized_payload`'s transitive-closure
/// copy of a THUNK (`X`) whose own captures (`bodyInner`, `watchList`) were
/// bound OUTSIDE it, forced only AFTER tenure + a forced collection.
/// `watchList` itself is never read past that force.
#[test]
fn thunked_app_capture_survives_tenure_and_resume() {
    let table = table();
    let mut session = handled_session();

    let expr = build_wrap_suspend_with_thunked_app_capture(100, 1, 200);
    let outcome = session
        .run("wrap", &expr, &table)
        .unwrap_or_else(|e| panic!("wrap suspend run failed: {e}"));
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // Forcing X (applying bodyInner to watchList) happens HERE, inside
    // run_forked's own drive, forking the thread through its OWN suspend --
    // this is where tenure's transitive-closure copy of X (and X's own
    // captures bodyInner/watchList) gets exercised for real.
    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    let _ = &inner_hole; // left parked, never resumed

    session.force_gc_for_test();
    session.force_gc_for_test();

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| panic!("wrap resume failed: {e}"));
    let (down_back, up_back) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        (down_back, up_back),
        (111, 222),
        "the wrap continuation's own captured downMid/upMid must survive tenure of a \
         sibling closure whose ONLY capture is a pre-built thunk (X), matching \
         asyncSpawn's real \\_ -> x shape exactly"
    );
}

/// Repro #13 — the same `shared` free-variable shape as repro #1
/// (`build_wrap_suspend_shared`, `shared` captured by BOTH the tenured
/// `bodyClosure` AND the wrap's own independently-rooted continuation), but
/// with the `session.force_gc_for_test()` call between tenure and resume
/// REMOVED.
///
/// Every one of the twelve tests above this one calls `force_gc_for_test`
/// at least once in that window (grep confirms it) — none tested what
/// `OldSpace::tenure` promised on its own BEFORE the fix: the old
/// `cheney_copy` call walked ONLY the tenure root's (`bodyClosure`'s)
/// transitive graph (`old_space.rs`'s `tenure`, the single-element root
/// slice `&[&mut root as *mut *mut u8]`) — a SIBLING closure that
/// independently captured the SAME free variable was never visited by that
/// walk, so its own capture field still held `shared`'s PRE-tenure nursery
/// address, which `raw::evacuate` had physically overwritten with a
/// `TAG_FORWARDED` stub. Fixed by folding a real minor collection over every
/// ordinary root category into `tenure()` itself
/// (`run_minor_collection_for_tenure_fixup`, `host_fns/gc.rs`), reusing the
/// same forward-following mechanism proven correct in `old_space.rs`'s
/// `test_overlapping_tenures_preserve_sharing` — see
/// `run_minor_collection_for_tenure_fixup`'s doc for the full mechanism.
#[test]
fn shared_free_variable_stale_immediately_after_tenure_with_no_intervening_gc() {
    let table = table();
    let mut session = fresh_session();

    let expr = build_wrap_suspend_shared(100, 1, 200, 11);
    let outcome = session
        .run("wrap", &expr, &table)
        .expect("wrap suspend runs");
    let wrap_hole = match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("wrap suspend must suspend, got {other:?}"),
    };

    // Sentinel-tenure fires here: `bodyClosure`'s transitive closure --
    // including `shared` -- is evacuated into old-space. The wrap's OWN
    // parked continuation independently captured `shared` too; that copy is
    // NOT touched by this tenure call, and (unlike every test above) NO
    // subsequent collection runs to give it a chance to self-heal.
    let handle = session
        .finalized_handle(&wrap_hole)
        .unwrap_or_else(|| panic!("wrap frame carries no untaken body closure"));

    // No force_gc_for_test() here -- this is the point of the test.

    let outcome = session
        .resume(&wrap_hole, Value::Lit(Literal::LitInt(0)))
        .unwrap_or_else(|e| {
            panic!(
                "wrap resume failed: {e} -- if this is a case/shape trap reporting \
                 tag 255 (Forwarded), it confirms tenure() leaves a SIBLING closure's \
                 independently-captured reference to the tenured value stale, with no \
                 write-barrier/remembered-set entry recording the need for a later fixup"
            )
        });
    let (shared_back, _answer) = match outcome {
        ResidentOutcome::Completed { result, .. } => expect_wrap_pair(&result.into_value()),
        other => panic!("wrap resume must complete, got {other:?}"),
    };
    assert_eq!(
        shared_back, 999,
        "the wrap continuation's own (independently-captured) copy of `shared` must \
         survive tenure of a sibling closure that also captured it, even with NO \
         collection running in between"
    );

    // Consume `handle`'s RootCustody (matching repro #1's own discipline) --
    // the tenured closure must also still be independently usable.
    let inner_hole = match session
        .run_forked("thread", handle, RealmId(1), Some(&table))
        .unwrap_or_else(|e| panic!("run_forked failed: {e}"))
    {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("thread body must suspend on its own effect, got {other:?}"),
    };
    let inner_outcome = session
        .resume(&inner_hole, Value::Lit(Literal::LitInt(77)))
        .unwrap_or_else(|e| panic!("thread resume failed: {e}"));
    let answer = match inner_outcome {
        ResidentOutcome::Completed { result, .. } => match result.into_value() {
            Value::Con(id, ref fields) if id.0 == RESULT_ID.0 && fields.len() == 1 => {
                expect_int(&fields[0])
            }
            other => panic!("expected ThreadResult(answer), got {other:?}"),
        },
        other => panic!("thread must complete, got {other:?}"),
    };
    assert_eq!(
        answer, 77,
        "the tenured closure's own result must survive delivery"
    );
}
