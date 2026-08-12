//! Enforced constraint 1 (realm-lanes/B-prefix-compat, verdict §7 step 3):
//! entering the parked path with a realm whose non-empty handled prefix is
//! not EXACTLY EQUAL to the machine's established prefix — an empty prefix
//! is compatible with anything — must be refused loudly BEFORE the machine
//! is driven at all, and must leave the machine untouched. A strict
//! extension (agreeing up to the shorter length but differing in length) is
//! NOT compatible: the established prefix is caller-supplied metadata about
//! a realm's own suspend threshold, not a read of the machine's actual
//! (opaque, compile-time monomorphized) handler stack, so a shorter realm
//! says nothing about whether the real `H` has a handler at the extended
//! position. `refused_strict_extension` and
//! `refused_strict_extension_established_longer` below pin this.
//!
//! `DispatchEffect` is positional over an `HList` and the suspend test is
//! `tag >= suspend_tag`; both are correct only relative to ONE effect row
//! whose handled effects occupy a contiguous low prefix. The machine cannot
//! introspect its own handler stack (`H` is a compile-time monomorphized type
//! parameter), so it tracks an ESTABLISHED prefix instead — set from the
//! first non-empty handled prefix any realm ENTERS the parked path with, and
//! monotonic thereafter (never cleared, including on resume).
//!
//! The check runs at ENTRY, not at park: a parked-path turn that COMPLETES
//! without ever suspending still dispatches every one of its effects through
//! the machine's single `H`, exactly the same as one that suspends — that is
//! precisely the misroute surface the check exists to close, so checking
//! only realms that suspend (and only after they have already run) would
//! miss it entirely. `refused_disagreeing_completing_run_never_executes` and
//! `establishment_on_completion_then_refuses_disagreeing` below are the
//! cases that pin this.
//!
//! This file does not exercise dispatch itself (that machinery is unchanged
//! by this lane) — `handled_prefix` here is pure metadata threaded through
//! the park path, so most fragments reuse the same ASK_TAG=0 suspending
//! shape (varying only the `handled_prefix` argument), except the two cases
//! above which use a non-suspending fragment on purpose.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    ContinuationId, JitEffectMachine, JitError, ParkKind, ParkedOutcome, PrefixMismatch, RealmId,
    ResumeInput,
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

#[path = "support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
use session_scaffold_expect::expect_int;

// ─── freer-simple constructor IDs ──────────────────────────────────────────
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);
const C1: DataConId = DataConId(1);
const PAIR_ID: DataConId = DataConId(2);

/// The union tag every fragment in this file suspends at. Fixed at 0 because
/// this lane's check is pure metadata over `handled_prefix` — it does not
/// depend on `suspend_tag` matching `handled_prefix.len()` (that wiring is
/// lane §5-mechanical, out of scope here).
const ASK_TAG: u64 = 0;

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

/// `let captured = C1 CAPTURED_N in E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))`.
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

/// A parked-path turn that COMPLETES immediately without ever suspending:
/// `Val (C1 n)` — the freer-simple `Val` case (an immediate pure result), the
/// same union encoding `build_suspending_parent`'s continuation body returns
/// on ITS `Val` branch, just taken from the very first step instead of after
/// a resume. The suspendable driver expects every step's `Done` value in this
/// Val/E union shape — a bare `C1 n` (no `Val` wrapper) is not driveable
/// through `run_fragment_suspendable_parked` at all, which is why this is a
/// distinct builder from `session_scaffold_value::build_value_fragment` (built for
/// the plain, non-suspendable `run_fragment_pure` path instead).
fn build_completing_value(n: i64) -> CoreExpr {
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

/// Never dispatches — every fragment here suspends before reaching a handler.
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

fn assert_pair_result(v: &Value, expect_captured: i64, expect_answer: i64) {
    match v {
        Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
            assert_eq!(expect_int(&fields[0]), expect_captured);
            assert_eq!(expect_int(&fields[1]), expect_answer);
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

/// At every quiescent point on the parked path, registered stowed roots must
/// equal parked continuations.
fn assert_rooting_receipt(machine: &JitEffectMachine, expect: usize) {
    assert_eq!(machine.parked_count(), expect, "parked continuation count");
    assert_eq!(
        machine.stowed_roots_count(),
        expect,
        "every parked continuation must be a registered GC root for its whole parked lifetime"
    );
}

fn owned(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

/// Attempt to park a suspending fragment tagged with `handled_prefix`. Does
/// NOT unwrap — several cases in this file expect `Err`, and the caller
/// inspects it.
fn try_park_fragment(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    realm: RealmId,
    name: &str,
    captured: i64,
    req: i64,
    handled_prefix: &[String],
) -> Result<ContinuationId, JitError> {
    let func_id = machine
        .add_function(
            name,
            &build_suspending_parent(captured, req),
            table,
            &ExternalEnv::new(),
        )
        .expect("add suspending fragment");
    match machine.run_fragment_suspendable_parked(
        func_id,
        table,
        &mut NoDispatch,
        &(),
        ASK_TAG,
        realm,
        ParkKind::Plain,
        handled_prefix,
    )? {
        ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
            unreachable!(
                "this test parks only Plain/Binding turns - Project/Render \
                 completions cannot be produced for them"
            )
        }
        ParkedOutcome::Suspended { id, request, .. } => {
            assert_eq!(expect_int(&request), req, "fragment ask payload");
            Ok(id)
        }
        ParkedOutcome::Completed { .. } => panic!("the fragment should suspend at the ask"),
    }
}

/// Park a fragment expected to SUCCEED.
fn park_fragment(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    realm: RealmId,
    name: &str,
    captured: i64,
    req: i64,
    handled_prefix: &[String],
) -> ContinuationId {
    try_park_fragment(machine, table, realm, name, captured, req, handled_prefix)
        .unwrap_or_else(|e| panic!("park_fragment({name}) expected to succeed, got: {e}"))
}

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
        ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
            unreachable!(
                "this test parks only Plain/Binding turns - Project/Render \
                 completions cannot be produced for them"
            )
        }
        ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
    }
}

fn new_machine(table: &DataConTable) -> JitEffectMachine {
    JitEffectMachine::compile_session(&build_suspending_parent(0, 0), table, 1 << 16)
        .expect("compile_session")
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
// Accepted: empty <-> non-empty, both orders. Both orders matter — the
// established-prefix rule is asymmetric in its bookkeeping (which park
// actually sets it) even though the compatibility relation is symmetric.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn accepted_empty_then_nonempty() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &[]);
        assert_rooting_receipt(&machine, 1);

        let base = owned(&["FileIO", "Proc"]);
        let b = park_fragment(&mut machine, &table, RealmId(1), "b", 2, 2, &base);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, a, 1, 1);
        resume_and_verify(&mut machine, b, 2, 2);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

#[test]
#[serial]
fn accepted_nonempty_then_empty() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let base = owned(&["FileIO", "Proc"]);
        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &base);
        assert_rooting_receipt(&machine, 1);

        let b = park_fragment(&mut machine, &table, RealmId(1), "b", 2, 2, &[]);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, a, 1, 1);
        resume_and_verify(&mut machine, b, 2, 2);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Accepted: identical prefixes.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn accepted_identical_prefixes() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let base = owned(&["FileIO", "Proc"]);
        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &base);
        let b = park_fragment(&mut machine, &table, RealmId(1), "b", 2, 2, &base);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, a, 1, 1);
        resume_and_verify(&mut machine, b, 2, 2);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Refused: strict extension, both directions. Agreement up to the shorter
// length is NOT the check — two non-empty prefixes must be EXACTLY EQUAL.
// The established prefix is caller-supplied metadata about a realm's own
// suspend threshold; it says nothing about how many handlers the machine's
// real (opaque) `H` has, so a realm's shorter prefix does not license
// another realm to extend it — that extending realm's tag at the extended
// position sits BELOW ITS OWN threshold (dispatched, not suspended), and
// could reach a real handler `H` has there. Refusing on any length
// difference is what keeps this check sound.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn refused_strict_extension() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let short = owned(&["FileIO", "Proc"]);
        let long = owned(&["FileIO", "Proc", "Memory"]);

        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &short);
        assert_rooting_receipt(&machine, 1);

        let before_ids = machine.parked_ids();
        let before_parked = machine.parked_count();
        let before_roots = machine.stowed_roots_count();

        let err = try_park_fragment(&mut machine, &table, RealmId(1), "b_refused", 2, 2, &long)
            .expect_err("a strict extension of the established prefix must be refused");
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, short, "error must name the established prefix");
                assert_eq!(incoming, long, "error must name the incoming prefix");
                assert_eq!(
                    mismatch,
                    PrefixMismatch::Length,
                    "an extension disagrees in length, not at a shared position"
                );
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }

        // The machine is untouched by the refusal.
        assert_eq!(machine.parked_ids(), before_ids, "parked_ids unchanged");
        assert_eq!(
            machine.parked_count(),
            before_parked,
            "parked_count unchanged"
        );
        assert_eq!(
            machine.stowed_roots_count(),
            before_roots,
            "stowed_roots_count unchanged"
        );
        assert_rooting_receipt(&machine, 1);

        // A subsequent COMPATIBLE park still succeeds — the established
        // prefix was not corrupted by the refused attempt.
        let c = park_fragment(&mut machine, &table, RealmId(2), "c", 3, 3, &short);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, a, 1, 1);
        resume_and_verify(&mut machine, c, 3, 3);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

/// Mirror of `refused_strict_extension`: the ESTABLISHED prefix is the
/// longer one this time, and the shorter incoming prefix is refused too —
/// exact-length equality is symmetric even though which park sets the
/// established prefix is not.
#[test]
#[serial]
fn refused_strict_extension_established_longer() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let long = owned(&["FileIO", "Proc", "Memory"]);
        let short = owned(&["FileIO", "Proc"]);

        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &long);
        assert_rooting_receipt(&machine, 1);

        let before_ids = machine.parked_ids();
        let before_parked = machine.parked_count();
        let before_roots = machine.stowed_roots_count();

        let err = try_park_fragment(&mut machine, &table, RealmId(1), "b_refused", 2, 2, &short)
            .expect_err("an established prefix longer than the incoming one must be refused");
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, long, "error must name the established prefix");
                assert_eq!(incoming, short, "error must name the incoming prefix");
                assert_eq!(
                    mismatch,
                    PrefixMismatch::Length,
                    "an extension disagrees in length, not at a shared position"
                );
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }

        assert_eq!(machine.parked_ids(), before_ids, "parked_ids unchanged");
        assert_eq!(
            machine.parked_count(),
            before_parked,
            "parked_count unchanged"
        );
        assert_eq!(
            machine.stowed_roots_count(),
            before_roots,
            "stowed_roots_count unchanged"
        );
        assert_rooting_receipt(&machine, 1);

        let c = park_fragment(&mut machine, &table, RealmId(2), "c", 3, 3, &long);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, a, 1, 1);
        resume_and_verify(&mut machine, c, 3, 3);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Refused: disagreeing prefixes, both directions. Assert the error names the
// disagreeing position AND that the machine is untouched — parked_count,
// stowed_roots_count and parked_ids unchanged, and a subsequent COMPATIBLE
// park still succeeds (proving the refusal did not corrupt the established
// prefix).
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn refused_disagreeing_a_then_b() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let row_a = owned(&["FileIO", "Proc"]);
        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &row_a);
        assert_rooting_receipt(&machine, 1);

        let before_ids = machine.parked_ids();
        let before_parked = machine.parked_count();
        let before_roots = machine.stowed_roots_count();

        let row_b = owned(&["FileIO", "Memory"]);
        let err = try_park_fragment(&mut machine, &table, RealmId(1), "b_refused", 2, 2, &row_b)
            .expect_err("disagreeing prefix must be refused");
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, row_a, "error must name the established prefix");
                assert_eq!(incoming, row_b, "error must name the incoming prefix");
                assert_eq!(
                    mismatch,
                    PrefixMismatch::Position(1),
                    "FileIO agrees at 0; Memory vs Proc disagrees at 1"
                );
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }

        // The machine is untouched by the refusal.
        assert_eq!(machine.parked_ids(), before_ids, "parked_ids unchanged");
        assert_eq!(
            machine.parked_count(),
            before_parked,
            "parked_count unchanged"
        );
        assert_eq!(
            machine.stowed_roots_count(),
            before_roots,
            "stowed_roots_count unchanged"
        );
        assert_rooting_receipt(&machine, 1);

        // A subsequent COMPATIBLE park still succeeds — the established
        // prefix was not corrupted by the refused attempt.
        let c = park_fragment(&mut machine, &table, RealmId(2), "c", 3, 3, &row_a);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, a, 1, 1);
        resume_and_verify(&mut machine, c, 3, 3);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

#[test]
#[serial]
fn refused_disagreeing_b_then_a() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let row_b = owned(&["FileIO", "Memory"]);
        let b = park_fragment(&mut machine, &table, RealmId(0), "b", 1, 1, &row_b);
        assert_rooting_receipt(&machine, 1);

        let before_ids = machine.parked_ids();
        let before_parked = machine.parked_count();
        let before_roots = machine.stowed_roots_count();

        let row_a = owned(&["FileIO", "Proc"]);
        let err = try_park_fragment(&mut machine, &table, RealmId(1), "a_refused", 2, 2, &row_a)
            .expect_err("disagreeing prefix must be refused");
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, row_b);
                assert_eq!(incoming, row_a);
                assert_eq!(mismatch, PrefixMismatch::Position(1));
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }

        assert_eq!(machine.parked_ids(), before_ids);
        assert_eq!(machine.parked_count(), before_parked);
        assert_eq!(machine.stowed_roots_count(), before_roots);
        assert_rooting_receipt(&machine, 1);

        let c = park_fragment(&mut machine, &table, RealmId(2), "c", 3, 3, &row_b);
        assert_rooting_receipt(&machine, 2);

        resume_and_verify(&mut machine, b, 1, 1);
        resume_and_verify(&mut machine, c, 3, 3);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Monotonic: the established prefix survives a resume to completion. Park a
// base realm, resume it to completion (registry now empty), THEN try to park
// a disagreeing prefix — still refused. This pins "monotonic, not cleared".
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn established_prefix_survives_resume_to_completion() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let base = owned(&["FileIO", "Proc"]);
        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &base);
        assert_rooting_receipt(&machine, 1);

        // Resume to completion — the registry drains to empty.
        resume_and_verify(&mut machine, a, 1, 1);
        assert_rooting_receipt(&machine, 0);
        assert_eq!(
            machine.parked_count(),
            0,
            "registry must be empty before the disagreeing attempt"
        );

        let disagreeing = owned(&["FileIO", "Memory"]);
        let err = try_park_fragment(
            &mut machine,
            &table,
            RealmId(1),
            "disagreeing_after_drain",
            2,
            2,
            &disagreeing,
        )
        .expect_err("the established prefix must survive the registry draining to empty");
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, base);
                assert_eq!(incoming, disagreeing);
                assert_eq!(mismatch, PrefixMismatch::Position(1));
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }
        // The refused attempt must not have parked anything.
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// The gap the check's original placement missed: a parked-path run whose
// turn COMPLETES without ever suspending dispatches every one of its effects
// through the machine's single `H`, the same as a realm that suspends. If
// the check only ran at park time (inside the suspend arm), an incompatible
// realm that happens to complete would run start to finish, unchecked. The
// check must run at ENTRY, before the machine is driven, so this can never
// happen — and it must ALSO establish at entry (not defer to a park that
// never comes), or a realm that completes with a non-empty prefix is never
// recorded, and a later disagreeing park is wrongly accepted.
// ───────────────────────────────────────────────────────────────────────────

/// The case that would have caught the original gap: on a machine with an
/// established prefix, attempt a parked run — one that would COMPLETE, not
/// suspend, if it were allowed to run at all — with a disagreeing prefix. It
/// must be refused before it runs, and the machine must be untouched.
#[test]
#[serial]
fn refused_disagreeing_completing_run_never_executes() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);

        let row_a = owned(&["FileIO", "Proc"]);
        let a = park_fragment(&mut machine, &table, RealmId(0), "a", 1, 1, &row_a);
        assert_rooting_receipt(&machine, 1);

        let before_ids = machine.parked_ids();
        let before_parked = machine.parked_count();
        let before_roots = machine.stowed_roots_count();

        // `build_completing_value` never suspends — an immediate `Val (C1 n)`
        // — so if the entry check did not run before the drive, this would
        // COMPLETE successfully, proving nothing wrong. With the check at
        // entry it must never run at all.
        let func_id = machine
            .add_function(
                "completing_disagreeing",
                &build_completing_value(999),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add non-suspending fragment");
        let row_b = owned(&["FileIO", "Memory"]);
        let err = machine
            .run_fragment_suspendable_parked(
                func_id,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(1),
                ParkKind::Plain,
                &row_b,
            )
            .expect_err("a disagreeing prefix must be refused even for a completing turn");
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, row_a);
                assert_eq!(incoming, row_b);
                assert_eq!(mismatch, PrefixMismatch::Position(1));
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }

        // The machine is untouched: nothing ran, so nothing changed.
        assert_eq!(machine.parked_ids(), before_ids, "parked_ids unchanged");
        assert_eq!(
            machine.parked_count(),
            before_parked,
            "parked_count unchanged"
        );
        assert_eq!(
            machine.stowed_roots_count(),
            before_roots,
            "stowed_roots_count unchanged"
        );
        assert_rooting_receipt(&machine, 1);

        resume_and_verify(&mut machine, a, 1, 1);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

/// The subtle (1)-and-(2) interaction: a realm that runs a non-suspending
/// parked turn with a non-empty prefix DOES establish it, even though it
/// never parks a frame — so a later disagreeing park is still refused.
#[test]
#[serial]
fn establishment_on_completion_then_refuses_disagreeing() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = new_machine(&table);
        assert_rooting_receipt(&machine, 0);

        let row_a = owned(&["FileIO", "Proc"]);
        let func_id = machine
            .add_function(
                "completing_establisher",
                &build_completing_value(42),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add non-suspending fragment");
        match machine
            .run_fragment_suspendable_parked(
                func_id,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(0),
                ParkKind::Plain,
                &row_a,
            )
            .expect("a non-suspending turn with a non-empty prefix must be allowed to complete")
        {
            ParkedOutcome::Completed { value, .. } => assert_eq!(expect_int(&value), 42),
            ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. } => {
                unreachable!(
                    "this test parks only Plain/Binding turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            ParkedOutcome::Suspended { .. } => panic!("build_completing_value never suspends"),
        }
        // Nothing was parked — a completing turn leaves no frame — but the
        // prefix must still be established.
        assert_rooting_receipt(&machine, 0);

        let row_b = owned(&["FileIO", "Memory"]);
        let err = try_park_fragment(&mut machine, &table, RealmId(1), "b_refused", 1, 1, &row_b)
            .expect_err(
            "the established prefix from the completed run must still refuse a disagreeing park",
        );
        match err {
            JitError::IncompatibleHandledPrefix {
                established,
                incoming,
                mismatch,
            } => {
                assert_eq!(established, row_a);
                assert_eq!(incoming, row_b);
                assert_eq!(mismatch, PrefixMismatch::Position(1));
            }
            other => panic!("expected IncompatibleHandledPrefix, got: {other}"),
        }
        assert_rooting_receipt(&machine, 0);

        // A subsequent COMPATIBLE park still succeeds against the prefix the
        // completed run established.
        let c = park_fragment(&mut machine, &table, RealmId(2), "c", 7, 7, &row_a);
        assert_rooting_receipt(&machine, 1);
        resume_and_verify(&mut machine, c, 7, 7);
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}
