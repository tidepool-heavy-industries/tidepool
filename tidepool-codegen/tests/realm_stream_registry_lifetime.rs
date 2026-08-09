//! REALM STREAM-REGISTRY LIFETIME — settles the codex-review-2026-08-08.md
//! item 3 HYPOTHESIS: "a continuation holding an unforced streamed-response
//! tail across a park by ANOTHER realm could reference a stream ID that the
//! other realm's teardown cleared."
//!
//! CONFIRMED REACHABLE, not merely hypothetical — and the mechanism is more
//! fundamental than "another realm": `parked_streams` (`MachineState`) is
//! machine-global, and PRE-FIX, `RegistryGuard::drop` (jit_machine.rs) cleared
//! it UNCONDITIONALLY at the end of every single run/resume call — including
//! the very call that just parked the entry. A continuation that crosses ANY
//! suspend boundary while holding an unforced streamed-response tail thunk —
//! whether that boundary is its OWN park returning, or (this file's scenario)
//! a SIBLING realm's run completing on the same machine in between — found
//! its `StreamId` gone the next time it forced the tail.
//!
//! NOT a memory-safety bug: `stream_next_id` (`MachineState`) never resets, so
//! a stale id can never alias a DIFFERENT, newer stream (no UB, no silently
//! wrong data), and the thunk entry points (`stream_chunk`/`stream_element`,
//! `host_fns/streaming.rs`) already treat a missing registry lookup as a
//! clean, named runtime error (`"registry entry missing (stale
//! continuation?)"`). It was a correctness/availability gap: a legitimate
//! continuation shape (partial consumption of a streamed effect result across
//! a suspend point) broke with a spurious error, on every occurrence,
//! deterministically — not a rare race.
//!
//! THE FIX (`jit_machine.rs`, `RegistryGuard`/`Drop for RegistryGuard`):
//! `clear_parked_streams()` now runs only when NOTHING is left suspended
//! anywhere on the machine (the continuation registry is empty AND the
//! single slot is unoccupied) — read off two new raw pointers the guard
//! captures into `JitEffectMachine::continuations`/`suspended_continuation`
//! at `install_registries_with_cancel_flag` time, mirroring
//! `NestedChildGuard`'s existing raw-pointer-into-`self` pattern (both are
//! locals in an enclosing `&mut self` call, so reading them at Drop time is
//! sound). A stream entry whose owning continuation is still parked (any
//! realm) now survives every intervening run; one abandoned mid-consumption
//! while ANOTHER continuation stays parked simply lingers — a bounded delay,
//! not a leak, same tolerance this codebase already accepts for
//! `ContinuationFrame::finalized_root` — until the map is genuinely
//! unreachable from any live continuation.
//!
//! RED/GREEN, confirmed by hand against `two_realms_sibling_teardown_does_not_
//! orphan_a_parked_streamed_tail` below (the negative direction — restoring
//! the unconditional clear — is not committed, matching this branch's
//! discipline for all such controls): RED reproduces deterministically with
//! `[JIT] runtime_error called: kind=2 (UserError) msg="effect result stream:
//! registry entry missing (stale continuation?)"` on realm A's resume; GREEN
//! is this file as committed, with the fix in place.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    JitEffectMachine, ParkKind, ParkedOutcome, RealmId, ResumeInput,
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

// Shared scaffold: C1 (DataConId(1)) and expect_int, same discipline as
// realm_multi_continuation.rs/realm_per_realm_fields.rs.
#[allow(dead_code)]
#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{expect_int, C1};

const PAIR_ID: DataConId = DataConId(2);
const CONS_ID: DataConId = DataConId(3);
const NIL_ID: DataConId = DataConId(4);
const I_HASH_ID: DataConId = DataConId(5);
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The internal (DISPATCHED, handled) effect realm A's entry issues before
/// its own suspending ask.
const STREAM_EFFECT_TAG: u64 = 0;
/// The suspending ask — the `suspend_tag` threshold for every call in this
/// file, dispatched-vs-suspend split at 1 (tag 0 dispatches, tag 1 suspends).
const ASK_TAG: u64 = 1;

fn table() -> DataConTable {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: PAIR_ID,
        name: "Pair".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    for (id, name, tag, arity) in [
        (CONS_ID, ":", 3u32, 2u32),
        (NIL_ID, "[]", 4, 0),
        (I_HASH_ID, "I#", 5, 1),
    ] {
        t.insert(DataCon {
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
        t.insert(DataCon {
            id,
            name: name.to_string(),
            tag: 0,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some(qual.to_string()),
            type_name: String::new(),
        });
    }
    t
}

/// ```text
/// E (Union (W# STREAM_EFFECT_TAG) (I# effectReq))
///   (Leaf (\v1 ->
///      E (Union (W# ASK_TAG) (I# askReq))
///        (Leaf (\v2 -> Val (Pair v1 (C1 v2))))))
/// ```
///
/// `v1` — the DISPATCHED effect's streamed answer — crosses the suspending
/// ask completely UNFORCED: nothing here pattern-matches or forces it before
/// parking, so at park time it is still a bare stream-tail thunk pointing
/// into `MachineState::parked_streams`.
fn build_streamed_tail_suspend(effect_req: i64, ask_req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    let var_v1 = b.push(CoreFrame::Var(VarId(0)));
    let var_v2 = b.push(CoreFrame::Var(VarId(1)));
    let c1_v2 = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v2],
    });
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_v1, c1_v2],
    });
    let val2 = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam2 = b.push(CoreFrame::Lam {
        binder: VarId(1),
        body: val2,
    });
    let leaf2 = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam2],
    });
    let ask_tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let ask_request = b.push(CoreFrame::Lit(Literal::LitInt(ask_req)));
    let ask_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![ask_tag_word, ask_request],
    });
    let e2 = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![ask_union, leaf2],
    });

    let lam1 = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: e2,
    });
    let leaf1 = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam1],
    });
    let effect_tag_word = b.push(CoreFrame::Lit(Literal::LitWord(STREAM_EFFECT_TAG)));
    let effect_request = b.push(CoreFrame::Lit(Literal::LitInt(effect_req)));
    let effect_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![effect_tag_word, effect_request],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![effect_union, leaf1],
    });
    b.build()
}

/// A trivial completing fragment for the SIBLING realm: `Val (C1 n)` — never
/// asks or dispatches an effect. Its only job is to be an unrelated run on
/// the same machine, so its `RegistryGuard::drop` is the exact mechanism
/// under test.
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

/// Dispatches `STREAM_EFFECT_TAG` with a lazily-streamed 5-element list —
/// takes the Park arm in `materialize_response_and_resume` unconditionally
/// (lazy results are on by default), which is what registers the entry in
/// `MachineState::parked_streams` this file is about.
struct StreamDispatch;
impl DispatchEffect<()> for StreamDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        assert_eq!(
            tag, STREAM_EFFECT_TAG,
            "only the internal effect should dispatch; the ask must suspend instead"
        );
        cx.respond_list(vec![1i64, 2, 3, 4, 5])
    }
}

/// Never dispatches — the sibling realm's fragment never issues an effect.
struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — the sibling realm's fragment never effects");
    }
}

/// Walk a `:`/`[]` cons chain (already forced by `heap_to_value_forcing`) and
/// assert it matches `expected` exactly.
fn assert_int_list(v: &Value, expected: &[i64]) {
    match v {
        Value::Con(id, fields) if id.0 == CONS_ID.0 && fields.len() == 2 => {
            let (head, rest) = expected
                .split_first()
                .unwrap_or_else(|| panic!("list longer than expected {expected:?}"));
            assert_eq!(expect_int(&fields[0]), *head, "streamed element mismatch");
            assert_int_list(&fields[1], rest);
        }
        Value::Con(id, fields) if id.0 == NIL_ID.0 && fields.is_empty() => {
            assert!(
                expected.is_empty(),
                "list shorter than expected, missing {expected:?}"
            );
        }
        other => panic!("expected a `:`/`[]` list cell, got {other:?}"),
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
// Realm A parks holding an unforced streamed tail. Realm B — a completely
// unrelated realm sharing the SAME machine — runs to completion three times
// in between (each its own `RegistryGuard`-scoped run/teardown). Realm A is
// then resumed and its streamed tail forced: pre-fix, ANY ONE of realm B's
// teardowns already orphaned realm A's stream entry (the very first one, in
// fact — the bug did not even need three).
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn two_realms_sibling_teardown_does_not_orphan_a_parked_streamed_tail() {
    in_test_thread(|| {
        tidepool_codegen::host_fns::reset_test_counters();
        let table = table();
        let mut machine = JitEffectMachine::compile_session(
            &build_streamed_tail_suspend(100, 7),
            &table,
            1 << 16,
        )
        .expect("compile_session");

        // Realm A: dispatch the internal streamed effect, then suspend on the
        // ask WITHOUT ever forcing the streamed answer.
        let id_a = match machine
            .run_suspendable_parked(&table, &mut StreamDispatch, &(), ASK_TAG, RealmId(0), &[])
            .expect("realm A run_suspendable_parked")
        {
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), 7, "realm A's own ask payload");
                id
            }
            ParkedOutcome::Completed { .. } => panic!("realm A must suspend on its own ask"),
        };
        assert_eq!(machine.parked_count(), 1);
        assert_eq!(machine.stowed_roots_count(), 1);

        // Realm B: three unrelated completing runs on the SAME machine, each
        // its own RegistryGuard scope — the sibling-realm teardown the
        // hypothesis names.
        for i in 0..3i64 {
            let frag = machine
                .add_function(
                    &format!("sibling_{i}"),
                    &build_val_fragment(900 + i),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add sibling fragment");
            match machine
                .run_fragment_suspendable_parked(
                    frag,
                    &table,
                    &mut NoDispatch,
                    &(),
                    ASK_TAG,
                    RealmId(1),
                    ParkKind::Plain,
                    &[],
                )
                .expect("sibling realm run")
            {
                ParkedOutcome::Completed { value, .. } => {
                    assert_eq!(expect_int(&value), 900 + i)
                }
                ParkedOutcome::Suspended { .. } => panic!("sibling fragment never asks"),
            }
            assert_eq!(
                machine.parked_count(),
                1,
                "realm A stays parked across every sibling-realm run"
            );
        }

        // Resume realm A and force the tail it held unforced across three
        // sibling-realm teardowns. Pre-fix this is exactly where
        // "registry entry missing (stale continuation?)" fired.
        match machine
            .resume_parked(
                id_a,
                &mut StreamDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(42))),
            )
            .expect("resume realm A")
        {
            ParkedOutcome::Completed { value, .. } => match &value {
                Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
                    assert_int_list(&fields[0], &[1, 2, 3, 4, 5]);
                    assert_eq!(expect_int(&fields[1]), 42, "resumed answer");
                }
                other => panic!("expected Pair(stream, C1 answer), got {other:?}"),
            },
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_eq!(machine.parked_count(), 0);
        assert_eq!(machine.stowed_roots_count(), 0);

        drop(machine);
    });
}
