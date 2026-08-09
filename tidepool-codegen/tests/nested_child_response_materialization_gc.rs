//! NurseryExhausted gc-retry CLASS load test for the effect-RESPONSE
//! materialization path inside `materialize_response_and_resume`
//! (`jit_machine.rs`), driven from a nested CHILD run against a suspended
//! parent (segment 40) — as opposed to `tests/nested_child_gc_rooting.rs`,
//! which exercises PURE nested-child value evaluation and never reaches
//! `materialize_response_and_resume` at all (its runs go through
//! `run_child_fragment_pure`, which never dispatches an effect).
//!
//! The scenario: a child fragment allocates a genuinely LIVE nested-Con chain
//! (kept alive by closing over it in its own continuation lambda — the same
//! "captured" shape `nested_child_gc_rooting.rs` uses to prove a stowed root
//! survives child GC), sized against a small nursery so it consumes nearly
//! all of it, then issues an effect request. The handler responds with
//! `Response::Stream`, which — with lazy results enabled (the default) —
//! takes `ResponsePlan::Park` in `materialize_response_and_resume` and calls
//! `host_fns::alloc_stream_tail_thunk`. With the live chain occupying nearly
//! the whole nursery, that allocation exhausts it, and — since the chain is
//! genuinely reachable (rooted via the child's own stowed continuation) — the
//! GC cannot reclaim it, so recovery needs `gc_trigger`'s heap-doubling growth.
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
//! HONEST LIMIT, found while tuning this test (not asserted, just recorded):
//! `alloc_stream_tail_thunk` was never a *zero*-retry site — it already goes
//! through `host_fns::gc::host_alloc_gc`, which had its own one-shot
//! gc-trigger-then-retry before this change (now itself routed through the
//! same shared `gc_retry` helper, ref STEP 4). Because `perform_gc`'s
//! heap-doubling always succeeds in a single collection below
//! `TIDEPOOL_MAX_HEAP` (`live_bytes` after one Cheney pass is bounded by the
//! from-space size, so it always fits the doubled space), a single retry
//! cycle at ANY layer is enough to rescue every depth this test could
//! reproduce — reverting ONLY the new outer wrap in `jit_machine.rs`'s Park
//! arm (leaving `host_alloc_gc`'s pre-existing inner retry intact) was tried
//! and did NOT reproduce failure at any tuned depth. The mutation this test
//! actually proves load-bearing is the CLASS-LEVEL one: neutering the shared
//! `gc_retry` helper itself (which both the pre-existing inner retry and the
//! new outer wrap now route through) reproduces
//! `Run(Jit(HeapBridge(NurseryExhausted)))` deterministically at
//! `CHAIN_DEPTH`/`NURSERY_BYTES` below. The outer wrap's independent value is
//! architectural, not independently reproducible under allocation pressure at
//! this scale: it keeps the Park arm consistent with the Eager arm's already-
//! fixed pattern, and it is what stops a FUTURE change (e.g. `alloc_stream_
//! tail_thunk` growing past a single-collection-fits-below-cap allocation, or
//! a refactor that drops `host_alloc_gc`'s own retry) from silently
//! reintroducing a zero-retry site at this call.
//!
//! ELIMINATION, for the next investigator who sees this failure resurface:
//! by the enumeration in this branch's commit message, every allocation site
//! reachable on the nested-child/stowed-continuation path was ALREADY
//! retry-protected before this branch (the Park arm was the sole asymmetry —
//! no OUTER retry, but transitively protected by `host_alloc_gc`'s inner
//! one) — none was a genuine zero-retry gap. That means the intermittent
//! `Run(Jit(HeapBridge(NurseryExhausted)))` reported on
//! `resident_session::nested_child_runs_while_parent_suspended_then_resumes`
//! is very likely NOT explained by a missing retry, and this branch likely
//! does NOT fix that intermittent — three green GHC runs (this branch's own
//! confirmation) don't clear an intermittent; repetition is the gate and the
//! rate sets the count, and three is not that count. Re-auditing retry
//! coverage on this path is a dead end for that failure; look elsewhere.
//! Hypotheses this branch's evidence does NOT eliminate (unconfirmed, not
//! investigated further here): something live across a retry that is not
//! actually registered as a GC root in the REAL resident-session shape
//! (unlike this test's synthetic one); heap-cap exhaustion where even
//! `perform_gc`'s doubling cannot satisfy the request (a real session's live
//! set, or `TIDEPOOL_MAX_HEAP`, may differ from what this test can reach);
//! or a distinct failure upstream of response materialization entirely that
//! only presents with this same error signature.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{JitEffectMachine, SuspendableOutcome};
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

/// The tag the CHILD's effect request carries. Any run with `suspend_tag =
/// None` (every plain/child run) dispatches every tag to the handler rather
/// than suspending, so this need not avoid `ASK_TAG` for correctness — kept
/// distinct anyway for clarity when reading a failure's tag in a log.
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
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — the ask should have suspended instead");
    }
}

fn suspend_parent(table: &DataConTable, nursery: usize, req: i64) -> JitEffectMachine {
    let entry = build_suspending_parent(req);
    let mut machine =
        JitEffectMachine::compile_session(&entry, table, nursery).expect("compile_session parent");
    let mut handler = NoDispatch;
    let outcome = machine
        .run_suspendable(table, &mut handler, &(), ASK_TAG)
        .expect("parent run_suspendable");
    assert!(matches!(outcome, SuspendableOutcome::Suspended { .. }));
    assert!(machine.is_suspended());
    machine
}

/// Build the CHILD's effect-dispatching fragment: a `depth`-deep nested `C1`
/// chain (genuinely live — closed over by the continuation lambda below, so
/// it survives the effect dispatch and any GC it forces), followed by an
/// effect request whose continuation pairs the captured chain with the
/// answer. Structurally identical in shape to
/// `nested_child_gc_rooting.rs::build_suspending_parent`'s captured-value
/// pattern, just with a deep chain instead of a single `C1 n` so its live
/// footprint is large and controllable via `depth`.
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

/// Responds to `EFFECT_TAG` with a lazily-streamed list — any size, even
/// this trivially small one, takes `ResponsePlan::Park` in
/// `materialize_response_and_resume` when lazy results are enabled (the
/// default), which is the arm under test.
struct StreamHandler;
impl DispatchEffect<()> for StreamHandler {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        assert_eq!(tag, EFFECT_TAG, "only the designated effect tag dispatches");
        cx.respond_list(vec![1i64, 2i64, 3i64])
    }
}

/// Depth of the live `C1` chain the child fragment builds, and the nursery
/// size it runs against. Both were found EMPIRICALLY (a scratch tuning scan
/// swept nursery/depth pairs and read `gc_retry_fired_count()`'s delta, plus
/// — separately — the exact error message under the class-level mutation
/// described above), not hand-derived from the heap-object byte layout: the
/// exact allocation size the compiled Con-constructor code emits, and how
/// much of the nursery the parent's own suspend + this child's own
/// effect-request scaffold consume before the live chain even starts, are
/// both internal to `emit/**` (out of this lane's scope) rather than stable,
/// cheap-to-derive constants. The execution is fully deterministic (no
/// timing/threading variance in the allocation sequence), so a pair confirmed
/// to land in the exhaustion window stays there on every future run against
/// an unchanged compiler — confirmed here over 20 repetitions via
/// `repeated_effect_response_park_arm_survives_nursery_exhaustion` (opt-in,
/// see below) with the fix in place, and by hand against the class-level
/// mutation (both directions, see the module doc). If a codegen change to
/// Con/thunk layout ever shifts this window, `retry_after == retry_before`
/// below fails loudly and names exactly what to retune.
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
    // unwrapped freer-simple `Val` payload (same shape
    // `nested_child_gc_rooting.rs::assert_pair_result` matches on directly
    // off `resume_suspended`'s outcome — no outer `Val` Con survives to the
    // bridged Rust `Value`).
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

/// Mutation-proof companion, gated OFF by default: 15+ repetitions of the
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
