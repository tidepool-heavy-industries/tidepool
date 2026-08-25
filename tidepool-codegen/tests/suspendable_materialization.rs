//! Suspend-then-complete under the `Project` and `Render` materialization
//! policies — the two the suspendable family did not have until
//! `finish_suspendable` stopped hand-rolling its own bind epilogue and started
//! calling `materialize`.
//!
//! Why these two are worth pinning:
//! the repl's multi-bind path (`(a, b) <- e`) and its bare-expression path
//! (`it` + render, the default for EVERY bare expression) run on exactly these
//! policies, and both must survive an `ask` mid-turn for the threadless
//! cutover to preserve behavior.
//!
//! The third test is the sharp one. `Render` bridges field 1 BEFORE tenuring
//! field 0 because, when `toWire` is the identity, the two fields are the SAME
//! heap object and the bridge is the owned deep copy that survives the
//! tenure's forwarding. That ordering now lives in ONE place
//! (`JitEffectMachine::materialize`) reached by both the suspending and the
//! non-suspending route, so this test asserts the property the shared code is
//! supposed to guarantee: an aliased `(x, x)` result is intact after a
//! suspension, a resume, and a later collection.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::heap_bridge;
use tidepool_codegen::jit_machine::{JitEffectMachine, ResumeInput, Suspendable};
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

// ─── freer-simple constructor ids ──────────────────────────────────────────
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The 2-field result tuple every policy under test projects/renders:
/// `Pair field0 field1`.
const PAIR_ID: DataConId = DataConId(2);

/// The union tag the suspend driver intercepts.
const ASK_TAG: u64 = 0;

fn synthetic_table() -> DataConTable {
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
    // ConTags::from_table is computed at compile time by every suspendable
    // entry, even for a fragment that never suspends — the full freer-simple
    // constructor set must be present or the entry fails closed with
    // MissingConTags before reaching the fragment.
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

/// `Val (C1 n)` — a plain non-suspending fragment, used as the session
/// machine's bootstrap entry and (later) as the allocating turn that forces a
/// collection over the tenured roots.
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

/// A SUSPENDING fragment whose continuation produces a 2-field tuple of two
/// DISTINCT objects:
///
/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
///
/// Field 0 is the pre-suspension capture, field 1 is built from the resume
/// answer — so a `Project` must tenure two different values, and a `Render`
/// must return field 1 while binding field 0.
fn build_suspending_pair(captured_n: i64, req: i64) -> CoreExpr {
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

/// The ALIASING shape: a SUSPENDING fragment whose continuation puts the SAME
/// heap object in both tuple fields — the `toWire = id` case
/// (`pure (it, toWire it)` where `toWire :: Value -> Value`):
///
/// ```text
/// E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> let x = C1 v in Val (Pair x x)))
/// ```
///
/// The aliasing is structural — one `LetNonRec` allocation, referenced twice —
/// and was confirmed against the emitter rather than assumed, by printing both
/// field pointers from `materialize`'s Render arm while these tests ran:
///
/// ```text
/// [PROBE] render field0=0x7f927802a960 field1=0x7f927802a960 aliased=true   (this fixture)
/// [PROBE] render field0=0x7f212c02a8c8 field1=0x7f212c02a9c0 aliased=false  (build_suspending_pair)
/// ```
///
/// The public API exposes no pointer identity, so that check cannot live in
/// the test body; this is its receipt.
fn build_suspending_aliased(req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let c1_v = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v],
    });
    let var_x_a = b.push(CoreFrame::Var(VarId(3)));
    let var_x_b = b.push(CoreFrame::Var(VarId(3)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_x_a, var_x_b],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let let_x = b.push(CoreFrame::LetNonRec {
        binder: VarId(3),
        rhs: c1_v,
        body: val,
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: let_x,
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
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
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
        panic!("handler dispatched tag {tag} — this fragment should have suspended instead");
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

fn in_test_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// Compile a session machine on the plain bootstrap entry, then add `expr` as
/// a fragment and return both.
fn session_with_fragment(
    table: &DataConTable,
    name: &str,
    expr: &CoreExpr,
) -> (JitEffectMachine, tidepool_codegen::jit_machine::FuncId) {
    let mut machine = JitEffectMachine::compile_session(&build_val_fragment(0), table, 1 << 16)
        .expect("compile_session");
    let func_id = machine
        .add_function(name, expr, table, &ExternalEnv::new())
        .expect("add suspending fragment");
    (machine, func_id)
}

/// Assert the entry suspended, and that the bridged ask request reached the
/// caller. Generic over the completion product: `Project` and `Render` each
/// complete with what they actually produce, so there is no one `Value` type
/// to write here.
fn expect_suspended<T>(outcome: Suspendable<T>, expect_req: i64) {
    match outcome {
        Suspendable::Suspended { request, .. } => {
            assert_eq!(
                expect_int(&request),
                expect_req,
                "the bridged ask request must reach the caller"
            );
        }
        Suspendable::Completed(_) => {
            panic!("the fragment should suspend at the ask, not complete")
        }
    }
}

/// Assert the entry completed, returning the policy's own product.
fn expect_completed<T>(outcome: Suspendable<T>) -> T {
    match outcome {
        Suspendable::Completed(t) => t,
        Suspendable::Suspended { .. } => panic!("the resume should complete, not re-suspend"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Project — `(a, b) <- e` that asks mid-turn. Nothing is tenured by the
// suspending run; the eventual resume tenures ALL N fields, in field order.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn projected_turn_suspends_then_tenures_every_field_on_resume() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = synthetic_table();
        let (mut machine, fid) =
            session_with_fragment(&table, "multi_bind_ask", &build_suspending_pair(111, 42));

        let outcome = machine
            .run_fragment_suspendable_projected(fid, &table, &mut NoDispatch, &(), ASK_TAG, 2)
            .expect("projected suspendable run");
        expect_suspended(outcome, 42);
        assert!(machine.is_suspended());

        // The completion IS the products: one tenured root per field, in field
        // order. There is no result value at all — a projection has none, and
        // the outcome type no longer demands one.
        let slots = expect_completed(
            machine
                .resume_suspended_projected(
                    &table,
                    &mut NoDispatch,
                    &(),
                    ASK_TAG,
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(222))),
                    2,
                )
                .expect("projected resume"),
        );
        assert!(!machine.is_suspended());

        assert_eq!(slots.len(), 2, "one root per projected field");
        assert!(
            machine.take_last_bound_root().is_none(),
            "the single-root stash belongs to Bind, not Project"
        );
        assert_ne!(
            slots[0].addr(),
            slots[1].addr(),
            "distinct fields get distinct slots"
        );
        let field0 = unsafe { heap_bridge::heap_to_value(slots[0].current()) }.expect("bridge f0");
        let field1 = unsafe { heap_bridge::heap_to_value(slots[1].current()) }.expect("bridge f1");
        assert_eq!(
            expect_int(&field0),
            111,
            "field 0 is the value captured BEFORE the suspension"
        );
        assert_eq!(
            expect_int(&field1),
            222,
            "field 1 is built from the resume answer"
        );

        // Both roots are persistent — a later allocating turn's collection
        // must leave them readable.
        let churn = machine
            .add_function("churn", &build_val_fragment(9), &table, &ExternalEnv::new())
            .expect("add churn fragment");
        let _ = machine
            .run_fragment(churn, &table, &mut NoDispatch, &())
            .expect("post-resume turn");
        let field0 = unsafe { heap_bridge::heap_to_value(slots[0].current()) }.expect("re-bridge");
        let field1 = unsafe { heap_bridge::heap_to_value(slots[1].current()) }.expect("re-bridge");
        assert_eq!(expect_int(&field0), 111, "field 0 survives a later turn");
        assert_eq!(expect_int(&field1), 222, "field 1 survives a later turn");

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Render — the repl's bare-expression `it` path (`pure (it, toWire it)`) that
// asks mid-turn. The resume must hand back BOTH products: the rendered field 1
// as the completion value, and field 0's tenured root on the machine.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn render_turn_suspends_then_returns_render_and_field0_root() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = synthetic_table();
        let (mut machine, fid) =
            session_with_fragment(&table, "bare_expr_ask", &build_suspending_pair(111, 7));

        let outcome = machine
            .run_fragment_suspendable_render(fid, &table, &mut NoDispatch, &(), ASK_TAG, true)
            .expect("render suspendable run");
        expect_suspended(outcome, 7);
        assert!(machine.is_suspended());
        assert!(
            machine.take_last_bound_root().is_none(),
            "a suspended render turn must not have tenured field 0 yet"
        );

        // The completion carries BOTH products: field 0's tenured root (`it`
        // itself) and field 1's render, together.
        let (slot, rendered) = expect_completed(
            machine
                .resume_suspended_render(
                    &table,
                    &mut NoDispatch,
                    &(),
                    ASK_TAG,
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(222))),
                    true,
                )
                .expect("render resume"),
        );
        assert!(!machine.is_suspended());

        assert_eq!(
            expect_int(&rendered),
            222,
            "the completion's rendered half is field 1"
        );
        assert!(
            machine.take_last_bound_root().is_none(),
            "Render returns its root inline — nothing is stashed on the machine"
        );
        let bound = unsafe { heap_bridge::heap_to_value(slot.current()) }.expect("bridge field 0");
        assert_eq!(
            expect_int(&bound),
            111,
            "field 0 is the bound value, tenured — not the render"
        );

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Render, ALIASED — field 0 and field 1 are the SAME heap object (identity
// `toWire`), and the turn suspends in between. `materialize` bridges field 1
// into an owned deep copy BEFORE tenuring field 0, so the tenure's forwarding
// cannot corrupt the render; routing the suspendable path through that same
// function is what makes the property survive a suspension.
//
// Run under GC_POISON: a stale pointer the tenure left behind reads as tag
// 221 deterministically rather than intermittently working.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn render_aliased_fields_survive_a_suspension() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = synthetic_table();
        let (mut machine, fid) =
            session_with_fragment(&table, "aliased_ask", &build_suspending_aliased(5));

        let outcome = machine
            .run_fragment_suspendable_render(fid, &table, &mut NoDispatch, &(), ASK_TAG, true)
            .expect("aliased render suspendable run");
        expect_suspended(outcome, 5);

        let (slot, rendered) = expect_completed(
            machine
                .resume_suspended_render(
                    &table,
                    &mut NoDispatch,
                    &(),
                    ASK_TAG,
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(333))),
                    true,
                )
                .expect("aliased render resume"),
        );
        // The render is a complete owned copy taken BEFORE the tenure.
        assert_eq!(
            expect_int(&rendered),
            333,
            "the aliased render must be intact after field 0's tenure"
        );

        let bound =
            unsafe { heap_bridge::heap_to_value(slot.current()) }.expect("bridge the aliased root");
        assert_eq!(
            expect_int(&bound),
            333,
            "the tenured object is the same value the render copied"
        );

        // And the tenured root stays readable after a later turn collects.
        let churn = machine
            .add_function("churn", &build_val_fragment(9), &table, &ExternalEnv::new())
            .expect("add churn fragment");
        let _ = machine
            .run_fragment(churn, &table, &mut NoDispatch, &())
            .expect("post-resume turn");
        let bound = unsafe { heap_bridge::heap_to_value(slot.current()) }.expect("re-bridge");
        assert_eq!(
            expect_int(&bound),
            333,
            "the aliased tenured object survives a later collection"
        );

        disarm_gc_hazards();
        drop(machine);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// The new entries inherit every guard their siblings have: a run started while
// a continuation is already suspended is refused exactly as the plain and bind
// entries refuse it (the L7 assert in `run_suspendable_shared`).
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn projected_and_render_entries_refuse_an_already_suspended_machine() {
    in_test_thread(|| {
        arm_gc_hazards();
        let table = synthetic_table();
        let (mut machine, fid) =
            session_with_fragment(&table, "first_ask", &build_suspending_pair(111, 1));

        let outcome = machine
            .run_fragment_suspendable_projected(fid, &table, &mut NoDispatch, &(), ASK_TAG, 2)
            .expect("first projected run suspends");
        expect_suspended(outcome, 1);
        assert!(machine.is_suspended());

        for probe in ["projected", "render"] {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Each arm maps to `()` separately: the two entries now have
                // DIFFERENT completion types (roots vs root+render), which is
                // the point — only the refusal is being probed here.
                match probe {
                    "projected" => machine
                        .run_fragment_suspendable_projected(
                            fid,
                            &table,
                            &mut NoDispatch,
                            &(),
                            ASK_TAG,
                            2,
                        )
                        .map(|_| ()),
                    _ => machine
                        .run_fragment_suspendable_render(
                            fid,
                            &table,
                            &mut NoDispatch,
                            &(),
                            ASK_TAG,
                            true,
                        )
                        .map(|_| ()),
                }
            }));
            let payload = caught.expect_err(&format!(
                "the {probe} entry must refuse an already-suspended machine"
            ));
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            assert!(
                msg.contains("already suspended"),
                "expected the L7 already-suspended assert, got: {msg}"
            );
        }
        // The refusal left the continuation intact.
        assert!(machine.is_suspended());

        disarm_gc_hazards();
        drop(machine);
    });
}
