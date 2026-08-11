//! REALM FALSIFIER — many continuations parked in ONE machine, resumed out of
//! order, with GC forced between the parks. Tests the claim that PERMANENT
//! ROOTING — not the temporal argument the single-slot path relies on (safe
//! only because no GC can run on a suspended machine, or because a nested
//! child's continuation is a registered GC root) — is what keeps a parked
//! continuation safe: every parked continuation is a REGISTERED GC ROOT for
//! its whole parked lifetime.
//!
//! Every test runs under `TIDEPOOL_GC_POISON` + `TIDEPOOL_HEAP_VERIFY` with a
//! nursery small enough to collect for real, so a missed root surfaces as a
//! DETERMINISTIC poisoned tag 221 rather than a flaky segfault. Each case's
//! section comment below states whether it is a safety case (dies under the
//! negative control that deletes `register_stowed_root` in
//! `park_continuation`) or an ordering/bookkeeping case (stays green under
//! that control).
//!
//! `stowed_roots_count() == parked_count()` is asserted at every step — the
//! receipt that rooting, not luck or timing, protects the parked
//! continuations.

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

use tidepool_heap::layout as heap_layout;

use serial_test::serial;

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
#[path = "support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
#[path = "support/session_scaffold_gc_forcing.rs"]
mod session_scaffold_gc_forcing;
#[path = "support/session_scaffold_value.rs"]
mod session_scaffold_value;
use session_scaffold::C1;
use session_scaffold_expect::expect_int;
use session_scaffold_gc_forcing::build_gc_forcing_fragment;
use session_scaffold_value::build_value_fragment;

// ─── freer-simple constructor IDs (must match ConTags::from_table lookup) ────
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

// ─── shape-widening additions (W1-W4, plans/post-restart/codex-review-2026-08-08.md
// item 5) — distinct ids, disjoint from the F1-F4/A5 ids above. ────────────

/// list cons/nil, matching `EffectContext::respond_list`'s `":"`/`"[]"`
/// name+arity lookup — used by W2's streamed tail.
const CONS_ID: DataConId = DataConId(3);
const NIL_ID: DataConId = DataConId(4);
/// Boxed Int, what `ToCore for i64` wraps a streamed element in
/// (`respond_list`'s pull-time conversion) — used by W2.
const I_HASH_ID: DataConId = DataConId(5);
/// `FinalizeWith site closure` — a 2-field Con whose field 1 is a raw
/// closure. Used by W3.
const FINALIZE_ID: DataConId = DataConId(16);
/// 3-field constructor for W1/W2's deep-verify: `Triple captured mid answer`.
const TRIPLE_ID: DataConId = DataConId(17);

/// The internal (DISPATCHED) effect tag W1/W2 use before their own suspending
/// ask — distinct from `ASK_TAG` so a single `suspend_tag` threshold (1)
/// dispatches tag 0 and suspends tag 1.
const MID_EFFECT_TAG: u64 = 0;
const MID_ASK_TAG: u64 = 1;

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
    for (id, name, tag, arity) in [
        (CONS_ID, ":", 3u32, 2u32),
        (NIL_ID, "[]", 4, 0),
        (I_HASH_ID, "I#", 5, 1),
        (FINALIZE_ID, "FinalizeWith", 16, 2),
        (TRIPLE_ID, "Triple", 17, 3),
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

// ─── W1/W2 — a NESTED / mid-effect suspend: one DISPATCHED effect (handled,
// answered inline) precedes the suspending ask, so the parked continuation
// closes over both a pre-existing captured value AND the dispatched effect's
// materialized answer, not just a clean top-level ask. ────────────────────

/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# MID_EFFECT_TAG) (I# effectReq))
///     (Leaf (\v1 ->
///        E (Union (W# MID_ASK_TAG) (I# askReq))
///          (Leaf (\v2 -> Val (Triple captured (C1 v1) (C1 v2))))))
/// ```
///
/// `v1` — the DISPATCHED effect's materialized answer — is threaded into the
/// continuation exactly like `captured`: both must survive whatever GC runs
/// while the SECOND (suspending) effect is parked.
fn build_mid_effect_suspend(captured_n: i64, effect_req: i64, ask_req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });

    // Innermost: \v2 -> Val (Triple captured (C1 v1) (C1 v2))
    let var_v1 = b.push(CoreFrame::Var(VarId(0)));
    let c1_v1 = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v1],
    });
    let var_v2 = b.push(CoreFrame::Var(VarId(2)));
    let c1_v2 = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v2],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let triple = b.push(CoreFrame::Con {
        tag: TRIPLE_ID,
        fields: vec![var_captured, c1_v1, c1_v2],
    });
    let val2 = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![triple],
    });
    let lam2 = b.push(CoreFrame::Lam {
        binder: VarId(2),
        body: val2,
    });
    let leaf2 = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam2],
    });
    let ask_tag_word = b.push(CoreFrame::Lit(Literal::LitWord(MID_ASK_TAG)));
    let ask_request = b.push(CoreFrame::Lit(Literal::LitInt(ask_req)));
    let ask_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![ask_tag_word, ask_request],
    });
    let e2 = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![ask_union, leaf2],
    });

    // Outer: \v1 -> e2
    let lam1 = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: e2,
    });
    let leaf1 = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam1],
    });
    let effect_tag_word = b.push(CoreFrame::Lit(Literal::LitWord(MID_EFFECT_TAG)));
    let effect_request = b.push(CoreFrame::Lit(Literal::LitInt(effect_req)));
    let effect_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![effect_tag_word, effect_request],
    });
    let e1 = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![effect_union, leaf1],
    });

    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e1,
    });
    b.build()
}

/// W2's sibling of [`build_mid_effect_suspend`]: identical shape, except `v1`
/// (the dispatched effect's answer — a STREAMED list here) is embedded
/// UNFORCED as the Triple's field directly, not wrapped in `C1`. Forcing it
/// is left entirely to whoever deep-verifies the resumed result.
fn build_streamed_tail_suspend(captured_n: i64, effect_req: i64, ask_req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });

    // Innermost: \v2 -> Val (Triple captured v1 (C1 v2)) — v1 (the streamed
    // tail) is passed through UNFORCED, unlike build_mid_effect_suspend.
    let var_v1 = b.push(CoreFrame::Var(VarId(0)));
    let var_v2 = b.push(CoreFrame::Var(VarId(2)));
    let c1_v2 = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v2],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let triple = b.push(CoreFrame::Con {
        tag: TRIPLE_ID,
        fields: vec![var_captured, var_v1, c1_v2],
    });
    let val2 = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![triple],
    });
    let lam2 = b.push(CoreFrame::Lam {
        binder: VarId(2),
        body: val2,
    });
    let leaf2 = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam2],
    });
    let ask_tag_word = b.push(CoreFrame::Lit(Literal::LitWord(MID_ASK_TAG)));
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
    let effect_tag_word = b.push(CoreFrame::Lit(Literal::LitWord(MID_EFFECT_TAG)));
    let effect_request = b.push(CoreFrame::Lit(Literal::LitInt(effect_req)));
    let effect_union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![effect_tag_word, effect_request],
    });
    let e1 = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![effect_union, leaf1],
    });

    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e1,
    });
    b.build()
}

/// W3's shape: suspends on a closure-valued `finalize`. Reproduced here
/// (test binaries are separate crates, so this can't just import a sibling's
/// helper) because W3 additionally forces a real collection while parked:
///
/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# ASK_TAG) (FinalizeWith site (\v -> v)))
///     (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
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

/// Dispatches `MID_EFFECT_TAG` by echoing the request straight back as the
/// answer (no `ToCore`/table lookup needed) — panics on anything else,
/// including the ask tag (which must suspend, not dispatch).
struct MidEffectDispatch;
impl DispatchEffect<()> for MidEffectDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        assert_eq!(
            tag, MID_EFFECT_TAG,
            "only the internal effect should dispatch; the ask must suspend instead"
        );
        Ok(Response::Complete(request.clone()))
    }
}

/// Dispatches `MID_EFFECT_TAG` with a lazily-streamed 3-element list — any
/// size takes the Park arm in `materialize_response_and_resume` when lazy
/// results are enabled (the default), which is the mechanism W2 falsifies.
struct StreamDispatch;
impl DispatchEffect<()> for StreamDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        assert_eq!(
            tag, MID_EFFECT_TAG,
            "only the internal effect should dispatch; the ask must suspend instead"
        );
        cx.respond_list(vec![10i64, 20i64, 30i64])
    }
}

/// Deep-verify a resumed W1/W2 result: `Triple captured mid answer`.
fn assert_triple_captured_and_answer(v: &Value, expect_captured: i64, expect_answer: i64) -> Value {
    match v {
        Value::Con(id, fields) if id.0 == TRIPLE_ID.0 && fields.len() == 3 => {
            assert_eq!(
                expect_int(&fields[0]),
                expect_captured,
                "captured value (bound BEFORE either effect) must survive every \
                 collection that ran while the continuation was parked"
            );
            assert_eq!(
                expect_int(&fields[2]),
                expect_answer,
                "the resumed (suspending) ask's answer must be threaded through"
            );
            fields[1].clone()
        }
        other => panic!("expected Triple(captured, mid, answer), got {other:?}"),
    }
}

/// Walk a `:`/`[]` cons chain (post `heap_to_value_forcing`, so every tail is
/// already forced) and assert it matches `expected` exactly.
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
        .run_suspendable_parked(table, &mut NoDispatch, &(), ASK_TAG, realm, &[])
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
            &[],
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
// parent in the slot.
//
// codex-review-2026-08-08.md item 11: this used to be reachable — a caller
// could park a realm, then call a legacy slot-path entry (`run_suspendable`)
// successfully, reaching the mixed state, and only find out later when
// `resume_parked` panicked. `run_suspendable_shared`'s entry guard (the
// interim fix; the structural fix — registry-only suspension — is a separate
// future lane) now rejects the slot-path entry itself, cleanly, the moment
// the registry is non-empty, so the mixed state is unreachable through the
// public API at all. The test below exercises exactly that: the
// park-then-legacy-entry sequence is rejected AT ENTRY, not left to panic
// downstream at `resume_parked`.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn park_then_legacy_slot_suspend_is_rejected_cleanly_at_entry() {
    in_test_thread(|| {
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(11, 2), &table, 1 << 14)
                .expect("compile_session");

        // Park one continuation in the REGISTRY first.
        let parked = park_fragment(&mut machine, &table, RealmId(0), "registry_park", 22, 3);
        assert_rooting_receipt(&machine, 1);

        // A legacy slot-path entry, attempted while a realm is parked, must
        // be rejected cleanly — not panic, and not succeed into the mixed
        // state.
        let err = match machine.run_suspendable(&table, &mut NoDispatch, &(), ASK_TAG) {
            Ok(_) => panic!(
                "run_suspendable must not succeed while a continuation is parked in the registry"
            ),
            Err(e) => e,
        };
        assert!(
            format!("{err}").contains("parked") || format!("{err}").contains("registry"),
            "rejection must name the parked-registry cause, got: {err}"
        );

        // The machine is untouched by the rejected attempt: still not
        // suspended in the slot, the park is still there and still rooted.
        assert!(
            !machine.is_suspended(),
            "a rejected slot-path attempt must not have stowed a continuation"
        );
        assert_rooting_receipt(&machine, 1);
        assert_eq!(machine.parked_count(), 1, "the registry park is untouched");

        // The parked continuation is still resumable normally — the
        // rejection left the machine exactly as it was.
        resume_and_verify(&mut machine, parked, 3, 22);
        assert_rooting_receipt(&machine, 0);
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

// ───────────────────────────────────────────────────────────────────────────
// W1-W4 — the four continuation SHAPES codex-review-2026-08-08.md item 5
// found unfalsified: F1-F4/A5 above only ever capture a constructor chain
// across a clean top-level ask. Each W-case parks a DIFFERENT heap shape,
// forces a real collection (doubling, where applicable) while it is parked,
// resumes, and deep-verifies. See this file's negative-control run for which
// of these are genuine SAFETY cases (die under the control, like F3/F4) vs
// ordering/bookkeeping-only (stay green, like F1/F2) — labelled per case
// below once that run confirmed it.
// ───────────────────────────────────────────────────────────────────────────

// ─── W1 — NESTED / MID-EFFECT: parked partway through an effect sequence
// (one DISPATCHED effect answered inline, THEN a suspending ask), not at a
// clean top-level ask. SAFETY CASE — dies under the negative control: the
// parked continuation's `captured` value and the dispatched effect's
// materialized answer are both ordinary nursery allocations protected only
// by `register_stowed_root`, exactly like F3/F4's captured chain.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn w1_nested_mid_effect_continuation_parks_across_gc() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_mid_effect_suspend(4004, 55, 9), &table, 2048)
                .expect("compile_session");

        let id = match machine
            .run_suspendable_parked(
                &table,
                &mut MidEffectDispatch,
                &(),
                MID_ASK_TAG,
                RealmId(0),
                &[],
            )
            .expect("w1 entry run_suspendable_parked")
        {
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), 9, "the suspending ask's own payload");
                id
            }
            ParkedOutcome::Completed { .. } => {
                panic!("w1 entry should suspend at the ask, not complete")
            }
        };
        assert_rooting_receipt(&machine, 1);

        // Collect + double with the mid-effect continuation parked and
        // nothing else protecting it — same discipline as F3.
        force_gc_on(&mut machine, &table, "w1_collapse_1", 150);
        force_gc_on(&mut machine, &table, "w1_collapse_2", 200);
        assert!(
            tidepool_codegen::host_fns::gc_doubling_run_count() > 0,
            "W1 must trip the heap-doubling branch, not just a plain Cheney pass"
        );
        assert_rooting_receipt(&machine, 1);

        match machine
            .resume_parked(
                id,
                &mut MidEffectDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(77))),
            )
            .expect("resume w1")
        {
            ParkedOutcome::Completed { value, .. } => {
                assert_triple_captured_and_answer(&value, 4004, 77);
            }
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// W2 — LIST RESPONSE ACROSS PARK: the dispatched effect's answer is a list
// (`respond_list`, materialized eagerly as heap cons cells at dispatch time),
// embedded in the parked continuation and only consumed after resume. SAFETY
// CASE — dies under the negative control: the response cells are ordinary
// nursery allocations reachable only through the (unrooted-under-control)
// continuation, so they must survive a GC while parked.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn w2_streamed_response_tail_parks_across_gc() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine = JitEffectMachine::compile_session(
            &build_streamed_tail_suspend(5005, 66, 10),
            &table,
            2048,
        )
        .expect("compile_session");

        let id = match machine
            .run_suspendable_parked(
                &table,
                &mut StreamDispatch,
                &(),
                MID_ASK_TAG,
                RealmId(0),
                &[],
            )
            .expect("w2 entry run_suspendable_parked")
        {
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), 10, "the suspending ask's own payload");
                id
            }
            ParkedOutcome::Completed { .. } => {
                panic!("w2 entry should suspend at the ask, not complete")
            }
        };
        assert_rooting_receipt(&machine, 1);

        force_gc_on(&mut machine, &table, "w2_collapse_1", 150);
        force_gc_on(&mut machine, &table, "w2_collapse_2", 200);
        assert!(
            tidepool_codegen::host_fns::gc_doubling_run_count() > 0,
            "W2 must trip the heap-doubling branch, not just a plain Cheney pass"
        );
        assert_rooting_receipt(&machine, 1);

        match machine
            .resume_parked(
                id,
                &mut StreamDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(88))),
            )
            .expect("resume w2")
        {
            ParkedOutcome::Completed { value, .. } => {
                let streamed = assert_triple_captured_and_answer(&value, 5005, 88);
                assert_int_list(&streamed, &[10, 20, 30]);
            }
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// W3 — FINALIZED CLOSURE PARK: suspended on a closure-valued `finalize`
// (`ContinuationFrame::finalized_root` populated). SPLIT CASE: the finalized
// payload itself is tenured into old-space and persistent-rooted at SUSPEND
// time (`tenure_finalized_payload`, BEFORE `park_continuation` even runs) —
// that protection is independent of `register_stowed_root` and stays green
// under the negative control. The REST of the parked continuation (`captured`,
// still an ordinary nursery allocation) is a safety case exactly like F3/F4.
// See the negative-control run below for the confirmed per-assertion split.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn w3_finalized_closure_park_survives_gc_and_resume() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_finalize_suspend(6006, 300), &table, 2048)
                .expect("compile_session");

        let id = match machine
            .run_suspendable_parked(&table, &mut NoDispatch, &(), ASK_TAG, RealmId(0), &[])
            .expect("w3 entry run_suspendable_parked")
        {
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
            ParkedOutcome::Completed { .. } => panic!("w3 entry must suspend on the finalize"),
        };
        assert_rooting_receipt(&machine, 1);

        force_gc_on(&mut machine, &table, "w3_collapse_1", 150);
        force_gc_on(&mut machine, &table, "w3_collapse_2", 200);
        assert!(
            tidepool_codegen::host_fns::gc_doubling_run_count() > 0,
            "W3 must trip the heap-doubling branch, not just a plain Cheney pass"
        );
        assert_rooting_receipt(&machine, 1);

        // The finalized payload's OWN protection (old-space persistent root,
        // established before this frame was even parked) — confirm it is
        // still a live, correctly-tagged closure object, not poisoned.
        let finalized = machine
            .take_parked_finalized_root(id)
            .expect("w3 finalized root");
        let tag = unsafe { heap_layout::read_tag(finalized.current()) };
        assert_eq!(
            tag,
            heap_layout::TAG_CLOSURE,
            "finalized payload must still be a live closure after two collections"
        );
        assert_rooting_receipt(&machine, 1);

        // The frame itself is still parked and rooted — resume it. THIS is
        // what actually depends on `register_stowed_root`: `captured` never
        // went through old-space, it is an ordinary value threaded through
        // the parked continuation like every other case in this file.
        match machine
            .resume_parked(
                id,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(42))),
            )
            .expect("resume w3")
        {
            ParkedOutcome::Completed { value, .. } => assert_pair_result(&value, 6006, 42),
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// W4 — BINDING PARK: `ParkKind::Binding { forced }`, both `forced: true` and
// `forced: false`. SAFETY CASE — the tenure itself (`OldSpace::tenure`) only
// runs AFTER resume completes the turn; while parked, the frame's captured
// value is the same ordinary nursery allocation as every other case here.
// ───────────────────────────────────────────────────────────────────────────

fn w4_binding_park_case(forced: bool, captured_n: i64, req: i64, answer: i64) {
    in_test_thread(move || {
        arm_gc_hazards();
        let table = adversarial_table();
        let mut machine =
            JitEffectMachine::compile_session(&build_suspending_parent(1, 1), &table, 2048)
                .expect("compile_session");

        let frag = machine
            .add_function(
                "w4_bind_frag",
                &build_suspending_parent(captured_n, req),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add w4 fragment");
        let id = match machine
            .run_fragment_suspendable_parked(
                frag,
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(0),
                ParkKind::Binding { forced },
                &[],
            )
            .expect("w4 fragment run_fragment_suspendable_parked")
        {
            ParkedOutcome::Suspended { id, request, .. } => {
                assert_eq!(expect_int(&request), req);
                id
            }
            ParkedOutcome::Completed { .. } => panic!("w4 fragment should suspend at the ask"),
        };
        assert_rooting_receipt(&machine, 1);

        force_gc_on(&mut machine, &table, "w4_collapse_1", 150);
        force_gc_on(&mut machine, &table, "w4_collapse_2", 200);
        assert!(
            tidepool_codegen::host_fns::gc_doubling_run_count() > 0,
            "W4 (forced={forced}) must trip the heap-doubling branch, not just a plain \
             Cheney pass"
        );
        assert_rooting_receipt(&machine, 1);

        match machine
            .resume_parked(
                id,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(answer))),
            )
            .expect("resume w4")
        {
            ParkedOutcome::Completed { value, bound_root } => {
                assert_pair_result(&value, captured_n, answer);
                let root = bound_root.expect("a Binding park kind must return Some(bound_root)");
                let bridged =
                    unsafe { heap_bridge::heap_to_value(root.current()) }.expect("bridge w4 root");
                assert_pair_result(&bridged, captured_n, answer);
            }
            ParkedOutcome::Suspended { .. } => panic!("resume should complete, not re-suspend"),
        }
        assert_rooting_receipt(&machine, 0);

        disarm_gc_hazards();
        drop(machine);
    });
}

#[test]
#[serial]
fn w4a_binding_park_forced_true_survives_gc_and_resume() {
    w4_binding_park_case(true, 8008, 41, 9);
}

#[test]
#[serial]
fn w4b_binding_park_forced_false_survives_gc_and_resume() {
    w4_binding_park_case(false, 8009, 43, 13);
}
