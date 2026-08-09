//! REALM FALSIFIER — many continuations parked in ONE machine, resumed out of
//! order, with GC forced between the parks.
//!
//! The design claim under test: PERMANENT ROOTING REPLACES THE TEMPORAL
//! ARGUMENT. Today a stowed continuation is safe for two different reasons
//! depending on state — idle-suspended it is safe because no GC can run on a
//! suspended machine (the L7 `suspended_continuation.is_none()` asserts), and
//! while a nested child runs it is instead safe because it is a REGISTERED GC
//! ROOT. The realm generalization drops the temporal half entirely and roots
//! EVERY parked continuation for its whole parked lifetime.
//!
//! This file is the fast kill-or-confirm. It parks a parent AND a child that
//! itself suspends — exactly what `ChildSuspended` forbids one level up —
//! resumes them out of order, and forces real collections (including heap
//! doubling) between the parks. Every test runs under
//! `TIDEPOOL_GC_POISON` + `TIDEPOOL_HEAP_VERIFY` with a nursery small enough to
//! collect for real, so a missed root surfaces as a DETERMINISTIC poisoned tag
//! 221 rather than a flaky segfault.
//!
//! Cases:
//!   F1 — park A, park B (a fragment that also suspends), resume B then A.
//!   F2 — the same two parks resumed in the OTHER order (A then B).
//!   F3 — park A, park B, run a GC-forcing fragment that collects AND doubles
//!        the heap, THEN resume both.
//!   F4 — eight parks with distinct captured values, a GC forced between each,
//!        resumed in a fixed shuffled order.
//!
//! `stowed_roots_count() == parked_count()` is asserted at every step. That
//! equality is the receipt that rooting — not luck, not timing — is what
//! protects the parked continuations.
//!
//! WHICH CASES CARRY THE SAFETY CLAIM. Deleting the `register_stowed_root` call
//! in `park_continuation` (the negative control) kills F3, F4, and the A5 case
//! with a poisoned tag — but leaves F1 and F2 GREEN, because neither forces a
//! collection between its parks. F1/F2 test the registry's ordering and
//! bookkeeping; F3/F4 test memory safety. Read the results that way.
//!
//! The single-slot machinery (`suspended_continuation`, `run_child_fragment`,
//! `enter_nested_child`, the L7 asserts) is the CONTROL GROUP and is untouched;
//! `nested_child_gc_rooting.rs` remains its suite.

use tidepool_codegen::emit::ExternalEnv;
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

// The shared scaffold carries helpers this file does not need
// (`build_reference_fragment`); `#[path]` inclusion makes them look dead here.
#[allow(dead_code)]
#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{build_gc_forcing_fragment, build_value_fragment};
use session_scaffold::{expect_int, C1};

// ─── freer-simple constructor IDs (must match ConTags::from_table lookup) ────
// Identical to nested_child_gc_rooting.rs's table — same synthetic effect stack.
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The `Ask` union tag the suspend driver intercepts.
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

/// Build a SUSPENDING entry (identical shape to
/// `nested_child_gc_rooting::build_suspending_parent`):
///
/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
///
/// The continuation CLOSES OVER `captured` — a heap object allocated BEFORE the
/// suspension — so a resume after any later collection must read both the
/// (relocated) captured value and the answer correctly. That closed-over value
/// is what a missed root destroys.
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

/// Never dispatches — the ask suspends before reaching a handler. Panics if
/// called, which would mean the suspend branch was not taken.
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

/// Deep-verify a resumed result: `Pair (C1 captured) (C1 answer)`. Walks the
/// WHOLE structure — a missed or dangling root lands a garbage tag or a wrong
/// payload here.
fn assert_pair_result(v: &Value, expect_captured: i64, expect_answer: i64) {
    match v {
        Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
            assert_eq!(
                expect_int(&fields[0]),
                expect_captured,
                "captured value (closed over BEFORE the park) must survive every \
                 collection that ran while the continuation was parked"
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

/// Enable the poison + verify knobs and zero the counters. Every test in this
/// file calls this first: poison turns a missed root into a deterministic bad
/// tag, verify walks the post-GC to-space.
fn arm_gc_hazards() {
    tidepool_codegen::host_fns::set_gc_poison(true);
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::reset_test_counters();
}

fn disarm_gc_hazards() {
    tidepool_codegen::host_fns::set_gc_poison(false);
    tidepool_codegen::host_fns::set_heap_verify(false);
}

/// THE RECEIPT: at every quiescent point on the parked path, the number of
/// registered stowed roots must equal the number of parked continuations. If
/// this ever drops below, a parked continuation is unrooted and the next
/// collection frees it.
fn assert_rooting_receipt(machine: &JitEffectMachine, expect: usize) {
    assert_eq!(machine.parked_count(), expect, "parked continuation count");
    assert_eq!(
        machine.stowed_roots_count(),
        expect,
        "every parked continuation must be a REGISTERED GC root for its whole \
         parked lifetime — a count below the parked count means one is protected \
         by nothing"
    );
    assert!(
        !machine.is_suspended(),
        "the parked path must leave `suspended_continuation` empty so the plain \
         entries' L7 asserts keep passing"
    );
}

/// Park the machine's ENTRY (a suspending parent) into `realm`.
fn park_entry(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    realm: RealmId,
    expect_req: i64,
) -> ContinuationId {
    match machine
        .run_suspendable_parked(table, &mut NoDispatch, &(), ASK_TAG, realm)
        .expect("entry run_suspendable_parked")
    {
        ParkedOutcome::Suspended { id, request, .. } => {
            assert_eq!(
                expect_int(&request),
                expect_req,
                "the suspension request must carry the ask payload"
            );
            id
        }
        ParkedOutcome::Completed { .. } => {
            panic!("the entry should suspend at the ask, not complete")
        }
    }
}

/// Park a FRAGMENT that itself suspends — the case `ChildSuspended` forbids on
/// the single-slot path. Nothing here is a "child": on the parked path the
/// machine is not suspended, so this is an ordinary suspendable fragment run
/// that happens to leave a second continuation parked next to the first.
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
        )
        .expect("fragment run_fragment_suspendable_parked")
    {
        ParkedOutcome::Suspended { id, request, .. } => {
            assert_eq!(expect_int(&request), req, "fragment ask payload");
            id
        }
        ParkedOutcome::Completed { .. } => panic!("the fragment should suspend at the ask"),
    }
}

/// Resume a parked continuation with an Int answer and deep-verify its result.
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
        ParkedOutcome::Completed { value, .. } => {
            assert_pair_result(&value, expect_captured, answer)
        }
        ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
    }
}

/// Run a pure GC-forcing fragment through the PLAIN entry against a machine
/// holding parked continuations, and assert it really collected. The plain
/// entry is the point: with parked continuations the machine is not suspended,
/// so no nested-child mode is needed and the L7 assert passes.
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
        "'{name}' must force at least one REAL collection while continuations are \
         parked (before={before}, after={after}) — otherwise this test proves nothing"
    );
}

/// Every case runs on its own 8 MiB thread (the JIT recurses deeply).
fn in_test_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// F1 — two continuations parked in one machine (a parent AND a fragment that
//      itself suspends), resumed CHILD-FIRST.
//
//      An ORDERING test: nothing here forces a collection between the parks, so
//      the negative control leaves it green. F3/F4 carry the safety claim.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn f1_two_parks_resumed_child_first() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        // 2 KiB nursery: small enough that the fragment allocations collect.
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(777, 5), &table, 2048)
                .expect("compile_session");

        // Park A — the parent.
        let a = park_entry(&mut machine, &table, RealmId(0), 5);
        assert_rooting_receipt(&machine, 1);

        // Park B — a fragment that ALSO suspends. On the single-slot path this
        // is `ChildSuspended`; here it is just a second frame in the registry.
        let b = park_fragment(&mut machine, &table, RealmId(1), "child_b", 888, 6);
        assert_rooting_receipt(&machine, 2);
        assert_ne!(a, b, "distinct parks get distinct ids");
        assert_eq!(machine.parked_realm(a), Some(RealmId(0)));
        assert_eq!(machine.parked_realm(b), Some(RealmId(1)));
        assert_eq!(machine.parked_ids(), vec![a, b]);

        // Resume B first — OUT OF ORDER relative to the park sequence.
        resume_and_verify(&mut machine, b, 6, 888);
        assert_rooting_receipt(&machine, 1);

        // Then A. Its continuation has now survived: B's park, B's whole resume
        // run (which allocates and may collect), and B's teardown.
        resume_and_verify(&mut machine, a, 5, 777);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// F2 — the same two parks, resumed in the OTHER order (A then B). Ordering
//      test, same caveat as F1.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn f2_two_parks_resumed_parent_first() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(4242, 11), &table, 2048)
                .expect("compile_session");

        let a = park_entry(&mut machine, &table, RealmId(0), 11);
        assert_rooting_receipt(&machine, 1);
        let b = park_fragment(&mut machine, &table, RealmId(1), "child_b", 3131, 12);
        assert_rooting_receipt(&machine, 2);

        // Parent first this time: B stays parked across A's whole resume run.
        resume_and_verify(&mut machine, a, 11, 4242);
        assert_rooting_receipt(&machine, 1);
        assert_eq!(machine.parked_ids(), vec![b]);

        resume_and_verify(&mut machine, b, 12, 3131);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// F3 — THE KILLER: park A, park B, then force a real collection AND heap
//      doubling with both parked, THEN resume both.
//
//      A missed root shows up here as a poisoned tag 221 (deterministic) or a
//      wrong captured payload, not as a flaky segfault.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn f3_gc_and_heap_doubling_between_parks() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        // 2 KiB nursery + deep fillers => the live set stays high after the
        // first Cheney pass, tripping the doubling re-evacuate (live*4 >
        // size*3) — the same path continuation_gc_root.rs exercises. Every
        // doubling pass must re-evacuate and re-update BOTH parked roots.
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(12345, 7), &table, 2048)
                .expect("compile_session");

        let a = park_entry(&mut machine, &table, RealmId(0), 7);
        let b = park_fragment(&mut machine, &table, RealmId(0), "child_b", 54321, 8);
        assert_rooting_receipt(&machine, 2);

        // Collect + double with BOTH continuations parked and nothing else
        // protecting them.
        let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
        force_gc_on(&mut machine, &table, "collapse_1", 150);
        force_gc_on(&mut machine, &table, "collapse_2", 200);
        let gc_after = tidepool_codegen::host_fns::gc_trigger_call_count();
        assert!(
            gc_after >= gc_before + 2,
            "both fillers must collect (before={gc_before}, after={gc_after})"
        );
        // Receipts, not assumptions: the doubling branch really ran (so both
        // parked roots were re-evacuated and re-updated a second time within
        // one collection), and the post-GC verifier really walked to-space.
        // Both counters are process-wide and nextest gives each test its own
        // process, so a nonzero reading is this test's own work.
        assert!(
            tidepool_codegen::host_fns::gc_doubling_run_count() > 0,
            "the fillers must trip the heap-DOUBLING branch, not just a plain \
             Cheney pass — otherwise F3 is only F1 with extra allocation"
        );
        assert!(
            tidepool_codegen::host_fns::heap_verify_run_count() > 0,
            "TIDEPOOL_HEAP_VERIFY must actually have walked the post-GC to-space"
        );

        // The parks survived the collections as roots, not as luck.
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, b, 8, 54321);
        assert_rooting_receipt(&machine, 1);
        resume_and_verify(&mut machine, a, 7, 12345);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// F4 — eight parked continuations with distinct captured values, a GC forced
//      between each park, resumed in a fixed shuffled order.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn f4_eight_parks_gc_between_each_shuffled_resume() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(1000, 100), &table, 2048)
                .expect("compile_session");

        // Park 0 is the entry; parks 1..8 are suspending fragments. Captured
        // values are distinct so a cross-wired root shows as a wrong payload,
        // not just a crash.
        let mut parks: Vec<(ContinuationId, i64, i64)> = Vec::new(); // (id, captured, answer)
        let a = park_entry(&mut machine, &table, RealmId(0), 100);
        parks.push((a, 1000, 100));
        force_gc_on(&mut machine, &table, "between_0", 120);
        assert_rooting_receipt(&machine, 1);

        for i in 1..8i64 {
            let captured = 1000 + i * 111;
            let answer = 100 + i;
            let id = park_fragment(
                &mut machine,
                &table,
                RealmId(i as u64 % 3),
                &format!("park_{i}"),
                captured,
                answer,
            );
            parks.push((id, captured, answer));
            force_gc_on(&mut machine, &table, &format!("between_{i}"), 120);
            assert_rooting_receipt(&machine, parks.len());
        }
        assert_eq!(parks.len(), 8);
        assert!(
            tidepool_codegen::host_fns::heap_verify_run_count() > 0,
            "the post-GC verifier must have run across the eight parks"
        );

        // Fixed shuffle — no RNG, so a failure reproduces exactly.
        const ORDER: [usize; 8] = [3, 0, 7, 5, 1, 6, 2, 4];
        let mut remaining = parks.len();
        for &idx in ORDER.iter() {
            let (id, captured, answer) = parks[idx];
            resume_and_verify(&mut machine, id, answer, captured);
            remaining -= 1;
            assert_rooting_receipt(&machine, remaining);
        }
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// A5 on the parked path: a bottom-bearing answer must leave the frame PARKED
// and ROOTED so the caller can retry — the registry's version of
// nested_child_gc_rooting's (d).
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn parked_bottom_answer_leaves_the_frame_parked_and_rooted() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        // 2 KiB nursery, same as the F-cases: the post-rejection filler below
        // must force a REAL collection with the rejected frame still parked.
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(555, 3), &table, 2048)
                .expect("compile_session");

        let a = park_entry(&mut machine, &table, RealmId(0), 3);
        assert_rooting_receipt(&machine, 1);

        let bottom = Value::Con(
            PAIR_ID,
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::ThunkRef(tidepool_eval::value::ThunkId(0)),
            ],
        );
        let err = match machine.resume_parked(a, &mut NoDispatch, &(), ResumeInput::Answer(bottom))
        {
            Ok(_) => panic!("a bottom answer must be rejected, not accepted"),
            Err(e) => e,
        };
        assert!(
            format!("{err}").contains("normal form") || format!("{err}").contains("bottom"),
            "rejection must name the NF/bottom cause, got: {err}"
        );

        // The frame is still parked AND still rooted — the retry is safe even
        // if a collection happens in between.
        assert_rooting_receipt(&machine, 1);
        force_gc_on(&mut machine, &table, "after_reject", 120);
        assert_rooting_receipt(&machine, 1);

        resume_and_verify(&mut machine, a, 3, 555);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// THE TWO PATHS MUST NOT MIX. A slot-held continuation is UNREGISTERED — the
// single-slot path protects it by the temporal argument alone. Driving a parked
// resume against it would run collections with an unrooted continuation live.
//
// This is what makes lifting the `ChildSuspended` wall a CONVERSION rather than
// an addition: a caller one level up cannot park only the child and leave the
// parent in the slot. The assert below is the mechanical form of that finding.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn parked_resume_while_the_slot_is_occupied_panics() {
    in_test_thread(|| {
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(11, 2), &table, 1 << 14)
                .expect("compile_session");

        // Park one continuation in the REGISTRY, then — through the nested-child
        // door — suspend a second into the SLOT, reaching the mixed state.
        let parked = park_fragment(&mut machine, &table, RealmId(0), "registry_park", 22, 3);
        assert_rooting_receipt(&machine, 1);

        let entry_out = machine
            .run_suspendable(&table, &mut NoDispatch, &(), ASK_TAG)
            .expect("slot-path suspend");
        assert!(matches!(
            entry_out,
            tidepool_codegen::jit_machine::SuspendableOutcome::Suspended { .. }
        ));
        assert!(machine.is_suspended(), "the slot now holds a continuation");
        assert_eq!(
            machine.parked_count(),
            1,
            "the registry park is untouched by the slot suspension"
        );

        // Resuming the PARKED one now must panic rather than silently run a
        // collection with the slot-held continuation unrooted.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = machine.resume_parked(
                parked,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(3))),
            );
        }));
        assert!(
            r.is_err(),
            "a parked resume with the slot occupied must panic — mixing the two              suspension paths leaves the slot-held continuation unprotected"
        );
    });
}

// ───────────────────────────────────────────────────────────────────────────
// The registry does not disturb the single-slot path's guard rail: an unknown
// id is a clean error, and a resumed id is never reused.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn resuming_an_unknown_or_already_resumed_id_errors_cleanly() {
    in_test_thread(|| {
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(1, 1), &table, 1 << 14)
                .expect("compile_session");

        let bogus = ContinuationId(9999);
        let err = machine
            .resume_parked(
                bogus,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(1))),
            )
            .expect_err("an unknown id must error, not panic");
        assert!(format!("{err}").contains("no continuation parked"));

        let a = park_entry(&mut machine, &table, RealmId(0), 1);
        resume_and_verify(&mut machine, a, 1, 1);
        assert_rooting_receipt(&machine, 0);

        // The id is consumed; resuming it again is the same clean error (ids
        // are never reused, so this can never alias a later park).
        let err = machine
            .resume_parked(
                a,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(1))),
            )
            .expect_err("a consumed id must error");
        assert!(format!("{err}").contains("no continuation parked"));

        // And a plain fragment still runs — the machine was never "suspended".
        let frag = machine
            .add_function(
                "plain",
                &build_value_fragment(4321),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add plain fragment");
        let v = machine
            .run_fragment_pure(frag)
            .expect("plain fragment runs");
        assert_eq!(expect_int(&v), 4321);

        drop(machine);
    });
}
