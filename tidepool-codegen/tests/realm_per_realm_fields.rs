//! Lane A — per-realm fields onto the frame (realm-verdict §7 step 2).
//!
//! `last_bound_root`, `suspended_finalized_root`, and `cancel_flag` used to be
//! MACHINE-LEVEL singletons written by whichever realm ran last. With two
//! realms live, realm B's bind would silently overwrite realm A's —
//! `materialize_binder` binding the WRONG value under realm A's name, with
//! nothing panicking or type-erroring (both are valid `RootSlot`s). This file
//! is the red-then-green receipt that moving those fields onto the frame
//! closes that hole (A1), plus the sibling A2/A3/A4 relocations.
//!
//! A1's RED run (against the old machine-level `last_bound_root`) failed with
//! the silent-wrong-value shape the fix targets, not a panic or a `None`:
//!
//! ```text
//! assertion `left == right` failed: materialize_binder must bind realm A's value under realm A's name
//!   left: 222
//!  right: 111
//! ```
//!
//! Every heap-touching test here runs under `TIDEPOOL_GC_POISON` +
//! `TIDEPOOL_HEAP_VERIFY`, asserting `stowed_roots_count() == parked_count()`
//! at every quiescent point.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::heap_bridge;
use tidepool_codegen::jit_machine::{
    ContinuationId, JitEffectMachine, ParkKind, ParkedOutcome, RealmId, ResumeInput,
};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

use serial_test::serial;

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
#[path = "support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
use session_scaffold::C1;
use session_scaffold_expect::expect_int;

// ─── freer-simple constructor IDs, plus FINALIZE_ID for A2. ────────────────
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// `FinalizeWith site closure` shape (W4) — a 2-field Con whose field 1 is a
/// raw closure. Structural only: `request_carries_closure_sentinel` and
/// `tenure_finalized_payload` key off shape (>= 2 fields, field 1 a
/// `TAG_CLOSURE` heap object), not this constructor's identity.
const FINALIZE_ID: DataConId = DataConId(16);

/// The `Ask`/finalize union tag the suspend driver intercepts.
const ASK_TAG: u64 = 0;

/// A 2-field pair constructor making a resumed result deeply verifiable:
/// `Pair captured answerWrapped`.
const PAIR_ID: DataConId = DataConId(2);

fn adversarial_table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: PAIR_ID,
        name: "Pair".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: FINALIZE_ID,
        name: "FinalizeWith".to_string(),
        tag: 3,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    // ConTags::from_table is computed unconditionally at compile time by the
    // suspendable path, even for a fragment that never suspends — the full
    // freer-simple constructor set must be present or every parked entry
    // fails closed with MissingConTags before reaching the fragment.
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

/// `Val (C1 n)` — a plain non-suspending bind fragment result. Every
/// effectful entry/fragment result must be `Val x` or `E req k` (the
/// freer-simple wrapper the driver's `step` classifies on); a bare `C1 n`
/// hits `UnexpectedConTag`.
fn build_val_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    let c1 = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![lit],
    });
    b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![c1],
    });
    b.build()
}

/// Build a SUSPENDING entry:
///
/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
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

/// Build a SUSPENDING entry on a closure-valued `finalize` (W4), for A2:
///
/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# ASK_TAG) (FinalizeWith site (\v -> v)))
///     (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
///
/// The request's field 1 is a raw closure (`\v -> v`), not data — the
/// tolerant bridge substitutes a `CLOSURE_SENTINEL` for it
/// (`has_finalized_closure = true`), and the real closure crosses by
/// reference via the finalized-root machinery (A2).
fn build_suspending_finalize(captured_n: i64, site: i64) -> CoreExpr {
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

/// Never dispatches — the ask/finalize suspends before reaching a handler.
/// Panics if called, which would mean the suspend branch was not taken.
struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — this fragment should have suspended instead");
    }
}

/// Deep-verify a resumed result: `Pair (C1 captured) (C1 answer)`.
fn assert_pair_result(v: &Value, expect_captured: i64, expect_answer: i64) {
    match v {
        Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
            assert_eq!(
                expect_int(&fields[0]),
                expect_captured,
                "captured value must survive the park"
            );
            assert_eq!(
                expect_int(&fields[1]),
                expect_answer,
                "resumed answer must be threaded through the continuation"
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

/// At every quiescent point, parked continuations and registered stowed roots
/// must be equal.
fn assert_rooting_receipt(machine: &JitEffectMachine, expect: usize) {
    assert_eq!(machine.parked_count(), expect, "parked continuation count");
    assert_eq!(
        machine.stowed_roots_count(),
        expect,
        "every parked continuation must be a REGISTERED GC root for its whole \
         parked lifetime"
    );
}

fn park_entry(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    realm: RealmId,
    expect_req: i64,
) -> ContinuationId {
    match machine
        .run_suspendable_parked(table, &mut NoDispatch, &(), ASK_TAG, realm, &[])
        .expect("entry run_suspendable_parked")
    {
        ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
            unreachable!(
                "this test parks only Plain/Binding turns - Project/Render \
                 completions cannot be produced for them"
            )
        }
        ParkedOutcome::Suspended { id, request, .. } => {
            assert_eq!(expect_int(&request), expect_req);
            id
        }
        ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. } => {
            panic!("the entry should suspend, not complete")
        }
    }
}

fn park_fragment(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    realm: RealmId,
    name: &str,
    captured: i64,
    req: i64,
) -> ContinuationId {
    let func_id = machine
        .add_function(
            name,
            &build_suspending_parent(captured, req),
            table,
            &ExternalEnv::new(),
        )
        .expect("add suspending fragment");
    match machine
        .run_fragment_suspendable_parked(
            func_id,
            table,
            &mut NoDispatch,
            &(),
            ASK_TAG,
            realm,
            ParkKind::Plain,
            &[],
        )
        .expect("fragment run_fragment_suspendable_parked")
    {
        ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
            unreachable!(
                "this test parks only Plain/Binding turns - Project/Render \
                 completions cannot be produced for them"
            )
        }
        ParkedOutcome::Suspended { id, request, .. } => {
            assert_eq!(expect_int(&request), req);
            id
        }
        ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. } => {
            panic!("the fragment should suspend")
        }
    }
}

/// Resume a parked continuation with an Int answer and deep-verify its
/// result. `resume_parked` takes NO table (A4) — the frame's own is what
/// decodes it.
fn resume_and_verify(
    machine: &mut JitEffectMachine,
    id: ContinuationId,
    answer: i64,
    expect_captured: i64,
) {
    match machine
        .resume_parked(
            id,
            &mut NoDispatch,
            &(),
            ResumeInput::Answer(Value::Lit(Literal::LitInt(answer))),
        )
        .unwrap_or_else(|e| panic!("resume_parked({id:?}) failed: {e}"))
    {
        ParkedOutcome::CompletedValue(value) | ParkedOutcome::CompletedBinding { value, .. } => {
            assert_pair_result(&value, expect_captured, answer)
        }
        ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
            unreachable!(
                "this test parks only Plain/Binding turns - Project/Render \
                 completions cannot be produced for them"
            )
        }
        ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
    }
}

fn in_test_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// A1 — last_bound_root: the registry path returns the tenured root INLINE in
// `ParkedOutcome::Completed`, never touching the machine-level
// `last_bound_root` singleton a second realm could overwrite.
//
// RED (against yesterday's machine-level field, before this file's sibling
// production commit): run realm A's bind to completion, do NOT drain, run
// realm B's bind to completion, THEN `take_last_bound_root()` — silently
// returns B's value where A's was expected:
//
//   assertion `left == right` failed: materialize_binder must bind realm A's value under realm A's name
//     left: 222
//    right: 111
//
// GREEN (this test): each realm's `bound_root` rides home on its OWN
// `ParkedOutcome::Completed`, so there is no shared field to race on at all —
// `take_last_bound_root()` stays `None` throughout the registry path.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn a1_bound_root_returns_inline_never_touches_the_machine_level_field() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_val_fragment(0), &table, 1 << 16)
                .expect("compile_session");
        assert_rooting_receipt(&machine, 0);

        let frag_a = machine
            .add_function(
                "bind_a",
                &build_val_fragment(111),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add bind_a");
        let outcome_a = machine
            .run_fragment_suspendable_parked(
                frag_a,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(0),
                ParkKind::Binding { forced: true },
                &[],
            )
            .expect("realm A bind completes");
        let (value_a, root_a) = match outcome_a {
            ParkedOutcome::CompletedBinding { value, root } => (value, root),
            ParkedOutcome::CompletedValue(_)
            | ParkedOutcome::CompletedProject { .. }
            | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only a ParkKind::Binding turn, which can only \
                     complete as CompletedBinding"
                )
            }
            ParkedOutcome::Suspended { .. } => panic!("realm A's bind fragment never asks"),
        };
        assert_eq!(expect_int(&value_a), 111);
        // Nothing was stashed on the machine to race on — the whole point.
        assert!(machine.take_last_bound_root().is_none());
        assert_rooting_receipt(&machine, 0);

        let frag_b = machine
            .add_function(
                "bind_b",
                &build_val_fragment(222),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add bind_b");
        let outcome_b = machine
            .run_fragment_suspendable_parked(
                frag_b,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(1),
                ParkKind::Binding { forced: true },
                &[],
            )
            .expect("realm B bind completes");
        let (value_b, root_b) = match outcome_b {
            ParkedOutcome::CompletedBinding { value, root } => (value, root),
            ParkedOutcome::CompletedValue(_)
            | ParkedOutcome::CompletedProject { .. }
            | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only a ParkKind::Binding turn, which can only \
                     complete as CompletedBinding"
                )
            }
            ParkedOutcome::Suspended { .. } => panic!("realm B's bind fragment never asks"),
        };
        assert_eq!(expect_int(&value_b), 222);
        assert!(machine.take_last_bound_root().is_none());
        assert_rooting_receipt(&machine, 0);

        // Distinct slots, EACH STILL bridging to its own realm's value — A's
        // root was never overwritten by B's later completion.
        assert_ne!(root_a.addr(), root_b.addr(), "each realm gets its own slot");
        let bridged_a =
            unsafe { heap_bridge::heap_to_value(root_a.current()) }.expect("bridge A's root");
        let bridged_b =
            unsafe { heap_bridge::heap_to_value(root_b.current()) }.expect("bridge B's root");
        assert_eq!(
            expect_int(&bridged_a),
            111,
            "A's slot must still hold A's value"
        );
        assert_eq!(
            expect_int(&bridged_b),
            222,
            "B's slot must still hold B's value"
        );

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// A2 — suspended_finalized_root: relocated onto the frame. Two realms parked
// on a closure-valued finalize each get their OWN slot; taking one leaves the
// other's frame parked and rooted.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn a2_finalized_root_is_per_frame_not_per_machine() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_finalize(10, 100), &table, 1 << 16)
                .expect("compile_session");
        assert_rooting_receipt(&machine, 0);

        let id_a = match machine
            .run_suspendable_parked(&table, &mut NoDispatch, &(), ASK_TAG, RealmId(0), &[])
            .expect("realm A parks on its closure-valued finalize")
        {
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended {
                id,
                has_finalized_closure,
                ..
            } => {
                assert!(
                    has_finalized_closure,
                    "the request must carry a CLOSURE_SENTINEL for the closure field"
                );
                id
            }
            ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. } => {
                panic!("realm A must suspend on the finalize")
            }
        };
        assert_rooting_receipt(&machine, 1);

        let frag_b = machine
            .add_function(
                "finalize_b",
                &build_suspending_finalize(20, 200),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add finalize_b");
        let id_b = match machine
            .run_fragment_suspendable_parked(
                frag_b,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(1),
                ParkKind::Plain,
                &[],
            )
            .expect("realm B parks on its OWN closure-valued finalize")
        {
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended {
                id,
                has_finalized_closure,
                ..
            } => {
                assert!(has_finalized_closure);
                id
            }
            ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. } => {
                panic!("realm B must suspend on the finalize")
            }
        };
        assert_rooting_receipt(&machine, 2);
        assert_ne!(id_a, id_b);

        // Taking A's finalized root must not disturb B's frame at all.
        let slot_a = machine
            .take_parked_finalized_root(id_a)
            .expect("A's finalized root");
        assert_rooting_receipt(&machine, 2);
        assert!(
            machine.take_parked_finalized_root(id_a).is_none(),
            "a second take on the same id is None — the frame's handle is cleared, \
             not re-derived"
        );
        assert_rooting_receipt(&machine, 2);

        let slot_b = machine
            .take_parked_finalized_root(id_b)
            .expect("B's finalized root — its OWN, not A's");
        assert_ne!(
            slot_a.addr(),
            slot_b.addr(),
            "each realm's finalized payload tenures to its own slot"
        );
        assert_rooting_receipt(&machine, 2);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// A3 — cancel_flag: per realm, not per machine. Cancelling realm A must not
// abort realm B's resume on the same machine; resuming realm A afterward
// observes the cancellation it requested.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn a3_cancel_is_realm_scoped_resuming_a_sibling_realm_is_unaffected() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        // A small nursery: the resumed continuation's own allocation must
        // trip `gc_trigger`'s heap check for the cancel safepoint to be
        // reachable — a roomy nursery would let it complete without ever
        // consulting the flag. Not so small that the answer's OWN bridging
        // (materialize_response_and_resume, which runs BEFORE the
        // continuation and is not itself a cancel safepoint) exhausts the
        // nursery first — that surfaces as a `NurseryExhausted` bridge error
        // instead, an unrelated OOM rather than the cancellation this test
        // targets.
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(1, 1), &table, 300)
                .expect("compile_session");

        let a = park_entry(&mut machine, &table, RealmId(0), 1);
        assert_rooting_receipt(&machine, 1);
        let b = park_fragment(&mut machine, &table, RealmId(1), "b", 2, 2);
        assert_rooting_receipt(&machine, 2);

        machine.realm_cancel_handle(RealmId(0)).cancel();

        // Resuming a DIFFERENT realm completes normally — this is the whole
        // point of the flag moving off the machine and onto a per-realm map.
        resume_and_verify(&mut machine, b, 2, 2);
        assert_rooting_receipt(&machine, 1);

        // Resuming realm A now observes the cancellation IT requested.
        let err = machine
            .resume_parked(
                a,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(1))),
            )
            .expect_err("realm A's own cancellation must be observed on its resume");
        assert!(
            format!("{err}").to_lowercase().contains("cancel"),
            "rejection must name the cancellation cause, got: {err}"
        );
        assert_rooting_receipt(&machine, 0);

        // Not auto-cleared after the cancelled run (see `realm_cancel_handle`'s
        // doc for this choice) — a fresh handle for the same realm still
        // reads cancelled.
        assert!(
            machine.realm_cancel_handle(RealmId(0)).is_cancelled(),
            "a cancelled realm's flag stays set until the caller explicitly resets it"
        );

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// A4 — DataConTable: onto the frame; `resume_parked` no longer accepts one at
// all, so resuming a frame against a foreign row is impossible by
// construction. The frame's own table (captured at park time) is what
// decodes the resume.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn a4_resume_parked_uses_the_frames_own_table() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(9, 9), &table, 2048)
                .expect("compile_session");

        let a = park_entry(&mut machine, &table, RealmId(0), 9);
        assert_rooting_receipt(&machine, 1);

        // `resume_and_verify`/`resume_parked` supply no table at all — the
        // frame's own (cloned once at park time) is what the resume decodes
        // against, and the resumed turn behaves exactly as before.
        resume_and_verify(&mut machine, a, 9, 9);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}
