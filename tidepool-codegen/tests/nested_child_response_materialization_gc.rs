//! NurseryExhausted gc-retry regression for the effect-RESPONSE
//! materialization path inside `materialize_response_and_resume`
//! (`jit_machine.rs`), driven from a nested CHILD run against a suspended
//! parent — as opposed to `tests/nested_child_gc_rooting.rs`, which exercises
//! PURE nested-child value evaluation and never reaches
//! `materialize_response_and_resume` at all (its runs go through
//! `run_child_fragment_pure`, which never dispatches an effect).
//!
//! The scenario: a child fragment allocates a genuinely LIVE nested-Con chain
//! (kept alive by closing over it in its own continuation lambda), sized
//! against a small nursery so it consumes nearly all of it, then issues an
//! effect request. The handler responds via `respond_list`
//! (`Response::List`), which always takes the iterative
//! `ResponsePlan::Ready` arm in `materialize_response_and_resume`, allocating
//! through `host_fns::materialize_cons_list`. With the live chain occupying
//! nearly the whole nursery, that allocation exhausts it, and — since the
//! chain is genuinely reachable (rooted via the child's own stowed
//! continuation) — the GC cannot reclaim it, so recovery needs
//! `gc_trigger`'s heap-doubling growth.
//!
//! Reach/exercise is proven by an OBSERVABLE, not by trusting the test merely
//! passing: `heap_bridge::gc_retry_fired_count()` counts every time the
//! shared retry helper's exhaustion-and-retry branch actually ran. That
//! counter is untouched by ordinary JIT-compiled-code allocation (which
//! retries via its own emitted `gc_trigger` call, not through the Rust-side
//! `gc_retry` helper) — so a nonzero delta here is specific evidence that a
//! *host-side* retry-protected site on this path both exhausted on its first
//! attempt and used the retry.
//!
//! WHAT THIS TEST DOES NOT PROVE — for the next investigator who sees an
//! intermittent `Run(Jit(HeapBridge(NurseryExhausted)))` on
//! `resident_session::nested_child_runs_while_parent_suspended_then_resumes`.
//! Every allocation site reachable on the nested-child/stowed-continuation
//! path was already retry-protected when this test was written, so that
//! intermittent is very likely NOT a missing retry, and re-auditing retry
//! coverage here is a dead end. The mutation this test actually proves
//! load-bearing is the CLASS-LEVEL one: neutering the shared `gc_retry`
//! helper reproduces the failure deterministically at the `CHAIN_DEPTH` /
//! `NURSERY_BYTES` below. Hypotheses NOT eliminated: something live across a
//! retry that is not registered as a GC root in the REAL resident-session
//! shape (unlike this test's synthetic one); heap-cap exhaustion where even
//! `perform_gc`'s doubling cannot satisfy the request; or a distinct failure
//! upstream of response materialization presenting with the same signature.

mod support;
use support::LinearMachine;

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::suspension::SuspendableOutcome;
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

// ─── freer-simple constructor IDs (must match ConTags::from_table lookup) ────
const C1: DataConId = DataConId(1);
const PAIR_ID: DataConId = DataConId(2);
const CONS_ID: DataConId = DataConId(3);
const NIL_ID: DataConId = DataConId(4);
const I_HASH_ID: DataConId = DataConId(5);
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The `Ask` union tag the PARENT suspends on. Distinct from `EFFECT_TAG`
/// (the tag the CHILD dispatches through a real handler): the parent's own
/// suspend point plays no further role once the machine is parked — its only
/// job is to exist so `run_child_fragment` has a suspended parent to nest
/// under.
const ASK_TAG: u64 = 0;

/// The CHILD request's carrier tag. Rust routing ignores it; keeping it
/// distinct from the parent's tag makes the hand-built Core easier to inspect.
const EFFECT_TAG: u64 = 7;

fn table_with_list_cons() -> DataConTable {
    let mut table = DataConTable::new();
    for (id, name, tag, arity) in [
        (C1, "C1", 1u32, 1u32),
        (PAIR_ID, "Pair", 2, 2),
        (CONS_ID, ":", 3, 2),
        (NIL_ID, "[]", 4, 0),
        (I_HASH_ID, "I#", 5, 1),
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

/// Build the SUSPENDING parent entry — a bare-minimum ask, just enough to
/// give `run_child_fragment` a suspended machine to nest under. Its own
/// continuation plays no role in what this test is proving.
fn build_suspending_parent(req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![var_v],
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
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });
    b.build()
}

struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }
}

fn suspend_parent(table: &DataConTable, nursery: usize, req: i64) -> LinearMachine {
    let entry = build_suspending_parent(req);
    let mut machine = LinearMachine::new(
        JitEffectMachine::compile_session(&entry, table, nursery).expect("compile_session parent"),
    );
    let mut handler = NoDispatch;
    let outcome = machine
        .run_suspendable(table, &mut handler, &())
        .expect("parent run_suspendable");
    assert!(matches!(outcome, SuspendableOutcome::Suspended { .. }));
    assert!(machine.is_suspended());
    machine
}

/// Build the CHILD's effect-dispatching fragment: a `depth`-deep nested `C1`
/// chain (genuinely live — closed over by the continuation lambda below, so
/// it survives the effect dispatch and any GC it forces), followed by an
/// effect request whose continuation pairs the captured chain with the
/// answer.
fn build_effect_child(depth: usize, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    let mut node = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    for _ in 0..depth {
        node = b.push(CoreFrame::Con {
            tag: C1,
            fields: vec![node],
        });
    }
    let captured_chain = node;

    // \ans -> Val (Pair captured_chain (C1 ans))
    let var_ans = b.push(CoreFrame::Var(VarId(0)));
    let c1_ans = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_ans],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, c1_ans],
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

    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(EFFECT_TAG)));
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
        rhs: captured_chain,
        body: e,
    });
    b.build()
}

/// Responds to `EFFECT_TAG` with a list — any size, even this trivially
/// small one, takes the iterative `ResponsePlan::Ready` arm in
/// `materialize_response_and_resume` via `respond_list`, which is the arm
/// under test.
struct StreamHandler;
impl DispatchEffect<()> for StreamHandler {
    fn dispatch(
        &mut self,
        _request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Option<Response>, EffectError> {
        cx.respond_list(vec![1i64, 2i64, 3i64]).map(Some)
    }
}

/// Depth of the live `C1` chain the child fragment builds, and the nursery
/// size it runs against. Both were found EMPIRICALLY, not hand-derived from
/// the heap-object byte layout: the exact allocation size the compiled
/// Con-constructor code emits, and how much of the nursery the parent's own
/// suspend + this child's own effect-request scaffold consume before the
/// live chain even starts, are internal to `emit/**` rather than stable,
/// cheap-to-derive constants. Execution is fully deterministic, so a pair
/// confirmed to land in the exhaustion window stays there on every future
/// run against an unchanged compiler. If a codegen change to Con/thunk
/// layout ever shifts this window, `retry_after == retry_before` below fails
/// loudly and names exactly what to retune.
const CHAIN_DEPTH: usize = 115;
const NURSERY_BYTES: usize = 4096;

fn run_once() -> Result<(), String> {
    run_with(CHAIN_DEPTH, NURSERY_BYTES)
}

fn run_with(depth: usize, nursery: usize) -> Result<(), String> {
    tidepool_codegen::host_fns::reset_test_counters();
    tidepool_codegen::heap_bridge::reset_gc_retry_fired_count();

    let table = table_with_list_cons();
    let mut machine = suspend_parent(&table, nursery, 1);

    let child_expr = build_effect_child(depth, 2);
    let child = machine
        .add_function("effect_child", &child_expr, &table, &ExternalEnv::new())
        .expect("add effect child fragment");

    let mut handler = StreamHandler;
    let retry_before = tidepool_codegen::heap_bridge::gc_retry_fired_count();
    let result = machine.run_child_fragment(child, &table, &mut handler, &());
    let retry_after = tidepool_codegen::heap_bridge::gc_retry_fired_count();

    // The machine's `run`/`run_child_fragment` result is already the
    // unwrapped freer-simple `Val` payload — no outer `Val` Con survives to
    // the bridged Rust `Value`.
    let v = result.map_err(|e| format!("{e}"))?;
    match v {
        Value::Con(id, ref fields) if id == PAIR_ID && fields.len() == 2 => {
            // fields[0] is the (huge) captured chain; fields[1] is
            // `C1 <the streamed answer list>` — just confirm the answer
            // arrived wrapped as expected, without walking the whole list.
            match &fields[1] {
                Value::Con(id, f) if *id == C1 && f.len() == 1 => {}
                other => return Err(format!("expected C1 <answer>, got {other:?}")),
            }
        }
        other => return Err(format!("expected Pair(chain, C1 <answer>), got {other:?}")),
    }

    if retry_after == retry_before {
        return Err(format!(
            "gc_retry never fired (before={retry_before} after={retry_after}) — \
             the Park-arm allocation did not actually exhaust the nursery on this \
             run; CHAIN_DEPTH/NURSERY_BYTES need retuning"
        ));
    }
    assert!(
        machine.is_suspended(),
        "parent stays suspended across the child"
    );
    drop(machine);
    Ok(())
}

#[test]
#[serial]
fn effect_response_park_arm_survives_nursery_exhaustion_under_live_pressure() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            run_once().expect("effect response materialization must survive nursery exhaustion");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Mutation-proof companion, gated OFF by default: 20 repetitions of the
/// same scenario, each in its own thread/process-counters reset, proving the
/// green outcome is not a one-shot fluke. Run explicitly:
/// `TIDEPOOL_RUN_REPETITION_PROOF=1 cargo nextest run -p tidepool-codegen \
///   -E 'test(repeated_effect_response_park_arm_survives)'`
#[test]
#[serial]
fn repeated_effect_response_park_arm_survives_nursery_exhaustion() {
    if std::env::var("TIDEPOOL_RUN_REPETITION_PROOF").is_err() {
        eprintln!(
            "SKIPPED (opt-in): set TIDEPOOL_RUN_REPETITION_PROOF=1 to run the 15x repetition proof"
        );
        return;
    }
    const REPS: usize = 20;
    for i in 0..REPS {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                run_once().unwrap_or_else(|e| panic!("repetition {i}/{REPS} failed: {e}"))
            })
            .unwrap()
            .join()
            .unwrap();
    }
    eprintln!("{REPS}/{REPS} repetitions green");
}
