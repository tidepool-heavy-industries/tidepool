//! HANDLE + SCOPE-EXIT FALSIFIER (one-session plan, Phase 0.4) — the embedder
//! handle API (`ValueHandle`) and structured scope exit (`close_realm`) under
//! adversarial GC, on one machine holding multiple parked continuations.
//!
//! The claims under test, each of which a missed root or a botched release
//! turns into a deterministic poisoned tag 221 (never a flaky segfault):
//!
//! - a closure finalized by one parked frame ("the answerer") can be MINTED as
//!   a handle, survive collections between mint and delivery, be DELIVERED
//!   verbatim into a SIBLING parked frame ("the loop") via
//!   `ResumeInput::Handle`, and be APPLIED by the resumed continuation's own
//!   compiled code — the end-to-end pillar-B delivery the eager bridge's
//!   `CLOSURE_SENTINEL` makes impossible on the Value path;
//! - `observe_handle` is the one serialization seam: data bridges, a closure
//!   payload observes as the sentinel, and observation neither consumes the
//!   handle nor perturbs delivery (observe before AND after);
//! - `close_realm` is scope exit: the realm's frames and handles are released
//!   together, sibling realms are untouched, released ids error cleanly, and
//!   the rooting receipt holds before and after;
//! - `ParkKind::Project`/`ParkKind::Render` parks complete INLINE with their
//!   roots after collections, like `Binding` always has.
//!
//! Same scaffolding as `realm_multi_continuation.rs` (poison + verify armed,
//! tiny nursery, deep result verification).

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::heap_bridge::CLOSURE_SENTINEL;
use tidepool_codegen::jit_machine::{
    ContinuationId, JitEffectMachine, ParkKind, ParkedOutcome, RealmId, ResumeInput,
};
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

use tidepool_heap::layout as heap_layout;

use serial_test::serial;

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
#[path = "support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
#[path = "support/session_scaffold_gc_forcing.rs"]
mod session_scaffold_gc_forcing;
use session_scaffold::C1;
use session_scaffold_expect::expect_int;
use session_scaffold_gc_forcing::build_gc_forcing_fragment;

// ─── freer-simple constructor IDs (must match ConTags::from_table lookup) ────
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The `Ask` union tag the suspend driver intercepts.
const ASK_TAG: u64 = 0;

/// 2-field pair for deep verification: `Pair captured x`.
const PAIR_ID: DataConId = DataConId(2);
/// `FinalizeWith site closure` — field 1 is a raw closure.
const FINALIZE_ID: DataConId = DataConId(16);

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    for (id, name, tag, arity) in [
        (C1, "C1", 1u32, 1u32),
        (PAIR_ID, "Pair", 2, 2),
        (FINALIZE_ID, "FinalizeWith", 16, 2),
    ] {
        table.insert(DataCon {
            id,
            name: name.to_string(),
            tag,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
        });
    }
    for (id, name, qual, arity) in [
        (VAL_ID, "Val", "Control.Monad.Freer.Val", 1u32),
        (E_ID, "E", "Control.Monad.Freer.E", 2),
        (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
        (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
        (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
    ] {
        table.insert(DataCon {
            id,
            name: name.to_string(),
            tag: 0,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some(qual.to_string()),
            type_name: String::new(),
        });
    }
    table
}

/// A suspending program whose continuation returns data:
/// `let captured = C1 n in E (Union ASK_TAG (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))`.
fn build_suspending_parent(captured_n: i64, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });
    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let c1_v = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, c1_v],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val,
    });
    let leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam],
    });
    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let request = b.push(CoreFrame::Lit(Literal::LitInt(req)));
    let union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag_word, request],
    });
    let e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });
    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e,
    });
    b.build()
}

/// THE LOOP-SHAPED program: its continuation APPLIES the delivered answer —
/// `Leaf (\f -> Val (Pair captured (App f (C1 arg))))`. Resumed with a
/// delivered identity closure, it completes `Pair (C1 captured) (C1 arg)` —
/// the same shape `assert_pair_result` deep-verifies — proving the closure
/// crossed AND ran inside the resumed continuation's own compiled code.
fn build_applying_parent(captured_n: i64, req: i64, arg_n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });
    let arg_lit = b.push(CoreFrame::Lit(Literal::LitInt(arg_n)));
    let arg_con = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![arg_lit],
    });
    let var_f = b.push(CoreFrame::Var(VarId(0)));
    let applied = b.push(CoreFrame::App {
        fun: var_f,
        arg: arg_con,
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, applied],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val,
    });
    let leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam],
    });
    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let request = b.push(CoreFrame::Lit(Literal::LitInt(req)));
    let union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag_word, request],
    });
    let e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });
    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e,
    });
    b.build()
}

/// `let captured = C1 n in E (Union ASK_TAG (FinalizeWith site (\x -> x))) (Leaf (\v -> Val (Pair captured (C1 v))))`
/// — the answerer-shaped park: suspended on a closure-valued finalize.
fn build_finalize_suspend(captured_n: i64, site: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });
    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let c1_v = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, c1_v],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val,
    });
    let leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam],
    });
    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let site_lit = b.push(CoreFrame::Lit(Literal::LitInt(site)));
    let closure_var = b.push(CoreFrame::Var(VarId(2)));
    let closure = b.push(CoreFrame::Lam {
        binder: VarId(2),
        body: closure_var,
    });
    let finalize_with = b.push(CoreFrame::Con {
        tag: FINALIZE_ID,
        fields: vec![site_lit, closure],
    });
    let union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag_word, finalize_with],
    });
    let e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });
    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e,
    });
    b.build()
}

/// Never dispatches — every ask here suspends.
struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — the ask should have suspended instead");
    }
}

/// Deep-verify `Pair (C1 captured) (C1 answer)`.
fn assert_pair_result(v: &Value, expect_captured: i64, expect_answer: i64) {
    match v {
        Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
            assert_eq!(
                expect_int(&fields[0]),
                expect_captured,
                "captured value must survive every collection while parked"
            );
            assert_eq!(
                expect_int(&fields[1]),
                expect_answer,
                "the delivered/threaded answer must come through the continuation"
            );
        }
        other => panic!("expected Pair(C1 captured, C1 answer), got {other:?}"),
    }
}

fn arm_gc_hazards() {
    tidepool_codegen::host_fns::set_gc_poison(true);
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::reset_test_counters();
}

fn disarm_gc_hazards() {
    tidepool_codegen::host_fns::set_gc_poison(false);
    tidepool_codegen::host_fns::set_heap_verify(false);
}

/// The rooting receipt, handle-aware: parked count == stowed roots, and the
/// live handle count matches expectation.
fn assert_receipts(machine: &JitEffectMachine, parked: usize, handles: usize) {
    assert_eq!(machine.parked_count(), parked, "parked continuation count");
    assert_eq!(
        machine.stowed_roots_count(),
        parked,
        "every parked continuation must be a registered GC root for its whole parked lifetime"
    );
    assert_eq!(machine.value_handle_count(), handles, "live handle count");
    assert!(
        !machine.is_suspended(),
        "the parked path must leave the single slot empty"
    );
}

fn force_gc_on(machine: &mut JitEffectMachine, table: &DataConTable, name: &str, depth: usize) {
    let before = tidepool_codegen::host_fns::gc_trigger_call_count();
    let frag = machine
        .add_function(
            name,
            &build_gc_forcing_fragment(depth),
            table,
            &ExternalEnv::new(),
        )
        .expect("add gc-forcing fragment");
    let _ = machine
        .run_fragment_pure(frag)
        .expect("gc-forcing fragment runs against a machine holding parked continuations");
    let after = tidepool_codegen::host_fns::gc_trigger_call_count();
    assert!(
        after > before,
        "'{name}' must force at least one REAL collection while continuations are parked"
    );
}

fn in_test_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// Park the answerer-shaped finalize program as a fragment under `realm`,
/// asserting the closure sentinel rode the request.
fn park_finalize_fragment(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    realm: RealmId,
    name: &str,
    captured: i64,
    site: i64,
) -> ContinuationId {
    let frag = machine
        .add_function(
            name,
            &build_finalize_suspend(captured, site),
            table,
            &ExternalEnv::new(),
        )
        .expect("add finalize fragment");
    match machine
        .run_fragment_suspendable_parked(
            frag,
            table,
            &mut NoDispatch,
            &(),
            ASK_TAG,
            realm,
            ParkKind::Plain,
            &[],
        )
        .expect("park finalize fragment")
    {
        ParkedOutcome::Suspended {
            id,
            has_finalized_closure,
            ..
        } => {
            assert!(
                has_finalized_closure,
                "the finalize request must carry a CLOSURE_SENTINEL for the closure field"
            );
            id
        }
        other => panic!("finalize fragment must suspend, got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// H1 — THE PILLAR-B DELIVERY: an answerer realm's finalized closure, minted as
// a handle, observed (sentinel), delivered into the loop realm's parked frame,
// and APPLIED by the loop continuation's own compiled code — with forced
// collections between every step, and scope exit releasing the answerer realm
// afterward without touching the loop's completed world.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn h1_closure_handle_delivered_into_sibling_frame_and_applied() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = table();
        const LOOP_REALM: RealmId = RealmId(1);
        const ANSWERER_REALM: RealmId = RealmId(2);

        // The machine's entry IS the loop-shaped program: park it first.
        let mut machine =
            JitEffectMachine::compile_session(&build_applying_parent(7007, 55, 99), &table, 2048)
                .expect("compile_session");
        let loop_id = match machine
            .run_suspendable_parked(&table, &mut NoDispatch, &(), ASK_TAG, LOOP_REALM, &[])
            .expect("park loop entry")
        {
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), 55, "loop ask payload");
                id
            }
            other => panic!("loop entry must suspend, got {other:?}"),
        };
        assert_receipts(&machine, 1, 0);

        // The answerer parks beside it, suspended on a closure-valued finalize.
        let answerer_id = park_finalize_fragment(
            &mut machine,
            &table,
            ANSWERER_REALM,
            "answerer_finalize",
            6006,
            300,
        );
        assert_receipts(&machine, 2, 0);

        // Mint the handle. The answerer frame stays parked and rooted.
        let h = machine
            .handle_from_finalized(answerer_id)
            .expect("answerer frame holds a finalized payload");
        assert_eq!(machine.handle_realm(h), Some(ANSWERER_REALM));
        assert_receipts(&machine, 2, 1);
        // Minting is once-only: the frame's stash moved into the handle.
        assert!(
            machine.handle_from_finalized(answerer_id).is_none(),
            "a finalized payload mints exactly one handle"
        );

        // Collections between mint and delivery — the window where an unrooted
        // payload would be poisoned.
        force_gc_on(&mut machine, &table, "h1_collapse_1", 150);
        force_gc_on(&mut machine, &table, "h1_collapse_2", 200);
        assert_receipts(&machine, 2, 1);

        // OBSERVE before delivery: a closure payload observes as the sentinel
        // — the honest opaque view, never a lossy delivery.
        match &machine.observe_handle(h).expect("observe before delivery") {
            Value::Con(id, fields) => {
                assert_eq!(id.0, CLOSURE_SENTINEL.0, "closure observes as the sentinel");
                assert!(fields.is_empty());
            }
            other => panic!("expected the closure sentinel, got {other:?}"),
        }
        assert_receipts(&machine, 2, 1);

        // DELIVER: resume the LOOP frame with the handle. The loop's
        // continuation applies the delivered identity closure to `C1 99`
        // in compiled code — deep-verified via the Pair result.
        match machine
            .resume_parked(loop_id, &mut NoDispatch, &(), ResumeInput::Handle(h))
            .expect("resume loop with delivered closure")
        {
            ParkedOutcome::CompletedValue(value)
            | ParkedOutcome::CompletedBinding { value, .. } => assert_pair_result(&value, 7007, 99),
            other => panic!("loop resume must complete, got {other:?}"),
        }
        // Loop frame consumed; answerer frame still parked; handle NOT
        // consumed by delivery (scope-owned borrow).
        assert_receipts(&machine, 1, 1);

        // Observe AFTER delivery still works — the handle is a borrow.
        match &machine.observe_handle(h).expect("observe after delivery") {
            Value::Con(id, _) => assert_eq!(id.0, CLOSURE_SENTINEL.0),
            other => panic!("expected the closure sentinel, got {other:?}"),
        }

        // SCOPE EXIT: close the answerer realm — its parked finalize frame
        // and its handle go together; the receipt holds.
        let (frames, handles) = machine.close_realm(ANSWERER_REALM);
        assert_eq!((frames, handles), (1, 1));
        assert_receipts(&machine, 0, 0);

        // Released ids error cleanly — never a panic, never aliasing.
        assert!(
            machine
                .resume_parked(
                    answerer_id,
                    &mut NoDispatch,
                    &(),
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(1)))
                )
                .is_err(),
            "a closed realm's frame id must be a clean error"
        );
        assert!(
            machine.observe_handle(h).is_err(),
            "a released handle must be a clean error"
        );
        assert!(
            machine
                .resume_parked(loop_id, &mut NoDispatch, &(), ResumeInput::Handle(h))
                .is_err(),
            "delivering a released handle must be a clean error"
        );

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// H2 — SCOPE EXIT ISOLATION: two realms, one closed; the sibling's frame
// survives, resumes, and completes correctly after further collections.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn h2_close_realm_leaves_sibling_realm_untouched() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = table();
        const KEEP: RealmId = RealmId(10);
        const CLOSE: RealmId = RealmId(11);

        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(4321, 9), &table, 2048)
                .expect("compile_session");
        // KEEP realm: the entry.
        let keep_id = match machine
            .run_suspendable_parked(&table, &mut NoDispatch, &(), ASK_TAG, KEEP, &[])
            .expect("park keep entry")
        {
            ParkedOutcome::Suspended { id, .. } => id,
            other => panic!("keep entry must suspend, got {other:?}"),
        };
        // CLOSE realm: two fragments — one plain suspend, one finalize (whose
        // payload we deliberately leave UNTAKEN, exercising close_realm's
        // untaken-finalized-root release).
        let frag = machine
            .add_function(
                "close_plain",
                &build_suspending_parent(1111, 3),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add plain fragment");
        let _close_plain = match machine
            .run_fragment_suspendable_parked(
                frag,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                CLOSE,
                ParkKind::Plain,
                &[],
            )
            .expect("park plain fragment")
        {
            ParkedOutcome::Suspended { id, .. } => id,
            other => panic!("plain fragment must suspend, got {other:?}"),
        };
        let _close_fin = park_finalize_fragment(&mut machine, &table, CLOSE, "close_fin", 2222, 8);
        assert_receipts(&machine, 3, 0);

        force_gc_on(&mut machine, &table, "h2_collapse_1", 150);

        let (frames, handles) = machine.close_realm(CLOSE);
        assert_eq!((frames, handles), (2, 0));
        assert_receipts(&machine, 1, 0);

        // Closing an empty/unknown realm is a no-op, not an error.
        assert_eq!(machine.close_realm(RealmId(999)), (0, 0));

        // The sibling survives more collections and completes correctly.
        force_gc_on(&mut machine, &table, "h2_collapse_2", 200);
        match machine
            .resume_parked(
                keep_id,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(77))),
            )
            .expect("sibling resume")
        {
            ParkedOutcome::CompletedValue(value)
            | ParkedOutcome::CompletedBinding { value, .. } => assert_pair_result(&value, 4321, 77),
            other => panic!("sibling resume must complete, got {other:?}"),
        }
        assert_receipts(&machine, 0, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// H3 — PROJECT/RENDER PARKS: the two newly-spelled ParkKinds complete INLINE
// with their tenured roots after collections while parked.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn h3_project_and_render_parks_complete_inline() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(1, 1), &table, 2048)
                .expect("compile_session");

        // Project { n_fields: 2 } over the Pair-completing program.
        let frag = machine
            .add_function(
                "h3_project",
                &build_suspending_parent(31, 5),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add project fragment");
        let pid = match machine
            .run_fragment_suspendable_parked(
                frag,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(0),
                ParkKind::Project {
                    n_fields: std::num::NonZeroUsize::new(2).unwrap(),
                },
                &[],
            )
            .expect("park project fragment")
        {
            ParkedOutcome::Suspended { id, .. } => id,
            other => panic!("project fragment must suspend, got {other:?}"),
        };
        force_gc_on(&mut machine, &table, "h3_collapse_1", 150);
        match machine
            .resume_parked(
                pid,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(8))),
            )
            .expect("resume project park")
        {
            ParkedOutcome::CompletedProject { roots } => {
                assert_eq!(roots.len(), 2, "Pair projects two fields");
                for root in &roots {
                    // Each root is a live, correctly-tagged old-space object —
                    // a missed tenure or root shows up as poison here.
                    let tag = unsafe { heap_layout::read_tag(root.current()) };
                    assert_ne!(tag, 0xDD, "projected field poisoned — root lost");
                }
            }
            other => panic!("project park must complete as CompletedProject, got {other:?}"),
        }
        assert_receipts(&machine, 0, 0);

        // Render { field0_forced: true } over the same shape: field 1 bridges
        // to `C1 answer`, field 0 tenures.
        let frag2 = machine
            .add_function(
                "h3_render",
                &build_suspending_parent(41, 6),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add render fragment");
        let rid = match machine
            .run_fragment_suspendable_parked(
                frag2,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(0),
                ParkKind::Render {
                    field0_forced: true,
                },
                &[],
            )
            .expect("park render fragment")
        {
            ParkedOutcome::Suspended { id, .. } => id,
            other => panic!("render fragment must suspend, got {other:?}"),
        };
        force_gc_on(&mut machine, &table, "h3_collapse_2", 200);
        match machine
            .resume_parked(
                rid,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(12))),
            )
            .expect("resume render park")
        {
            ParkedOutcome::CompletedRender { root, rendered } => {
                let tag = unsafe { heap_layout::read_tag(root.current()) };
                assert_ne!(tag, 0xDD, "render field 0 poisoned — root lost");
                match &rendered {
                    Value::Con(id, fields) if id.0 == C1.0 && fields.len() == 1 => {
                        assert_eq!(expect_int(&fields[0]), 12, "field 1 renders the answer");
                    }
                    other => panic!("expected rendered C1 12, got {other:?}"),
                }
            }
            other => panic!("render park must complete as CompletedRender, got {other:?}"),
        }
        assert_receipts(&machine, 0, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}
