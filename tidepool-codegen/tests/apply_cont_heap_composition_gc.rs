//! Nursery-exhaustion regression for `apply_cont_heap`'s `E(union, k')`
//! continuation-composition loop (`tidepool-codegen/src/effect_machine.rs`).
//!
//! That loop rebuilds a `Node` chain from every pending `k2` on the Rust-heap
//! `k2_stack`, one `alloc_con` per entry, plus a final `alloc_con` for the
//! composed `E`. A `k2_stack` deep enough — e.g. from nested `mapM`/`foldM`
//! over many effectful iterations, exactly the shape
//! `tidepool-runtime/tests/nested_mapm_tag255.rs` stresses — can need more
//! Node allocations than a single nursery holds. `alloc_con`
//! was bump-only: nursery exhaustion mid-composition surfaced as a hard user
//! error ("apply_cont_heap: failed to allocate E result during continuation
//! composition"), not a stale-pointer read — the panic message on that test
//! misattributes it to the tag=255 forwarding-pointer bug (a distinct,
//! already-fixed failure mode; see that test's own header).
//!
//! This test builds a left-nested `Node` chain directly at the `CoreExpr`
//! level (bypassing GHC entirely, same idiom as
//! `tests/nested_child_response_materialization_gc.rs`) and runs it MANY
//! times on the SAME `compile_session` machine (whose heap persists across
//! `run` calls) against a small nursery. A single run's own construction
//! peak is sized to roughly match what its composition step needs — building
//! the whole chain up front and then re-composing an equal-sized one is a
//! structurally symmetric cost, so a single isolated run's construction
//! phase always leaves the nursery with enough post-GC headroom (the
//! collector's 4:3 heap-growth threshold overshoots on purpose) to cover its
//! OWN composition, and never reproduces exhaustion. Repetition breaks that
//! symmetry the same way the real `nested_mapm_tag255` shape does: nothing
//! between separate `run` calls forces a collection, so each run's discarded
//! heap (everything but the tiny bridged answer) sits as uncollected garbage
//! on top of the next run's — accumulating pressure across MANY small,
//! independent effect round-trips until SOME run's allocation, anywhere in
//! its turn, trips the nursery over. Reach is proven by an OBSERVABLE
//! (`heap_bridge::gc_retry_fired_count()`), not by trusting a green result:
//! that counter is untouched by ordinary JIT-compiled-code allocation (which
//! retries via its own emitted `gc_trigger` call, not through the Rust-side
//! `gc_retry` helper). `DEPTH` is sized so the composition loop's own
//! `alloc_con` calls dominate a run's total allocation (the list/primop
//! boxing retry sites are never reached by this scenario at all; the tiny
//! effect-response materialization is the only other reachable one, and is a
//! small, fixed cost per run against composition's `DEPTH`-scaling one) — so
//! a nonzero delta over the whole loop is strong evidence `alloc_con`'s
//! retry, not just SOME retry-protected site, exhausted and recovered.

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{Literal, TreeBuilder};

use serial_test::serial;

// ─── freer-simple constructor IDs (must match ConTags::from_table lookup) ────
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);
const I_HASH_ID: DataConId = DataConId(15);
const FIRST_REQUEST_ID: DataConId = DataConId(16);
const SECOND_REQUEST_ID: DataConId = DataConId(17);

/// The tag the ENTRY's own effect request carries — dispatched once, before
/// any continuation composition happens.
const EFFECT_TAG: u64 = 1;
/// The tag `f_e` (at the bottom of the deep `Node` chain) dispatches — its
/// response is what the composition loop is mid-way through delivering when
/// this test's nursery runs out.
const EFFECT_TAG2: u64 = 2;

fn table_with_freer_cons() -> DataConTable {
    let mut table = DataConTable::new();
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
    // The boxed-Int wrapper `ToCore`/`FromCore` bridges an `i64` through —
    // needed for `cx.respond(0i64)`/`cx.respond(99i64)` below.
    table.insert(DataCon {
        id: I_HASH_ID,
        name: "I#".to_string(),
        tag: 5,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    for (id, name) in [
        (FIRST_REQUEST_ID, "FirstRequest"),
        (SECOND_REQUEST_ID, "SecondRequest"),
    ] {
        table.insert(DataCon {
            id,
            name: name.to_string(),
            tag: 0,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(format!("Test.{name}")),
            type_name: "TestRequest".to_string(),
        });
    }
    table
}

/// Builds:
/// ```text
/// idLam    = \x -> Val x
/// fELam    = \y -> E (Union EFFECT_TAG2 y) (Leaf idLam)
/// k_0      = Leaf fELam
/// k_{i+1}  = Node k_i (Leaf idLam)             -- repeated `depth` times
/// entry    = E (Union EFFECT_TAG 0#) k_depth
/// ```
/// Running `entry` dispatches `EFFECT_TAG`, then `apply_cont_heap` descends
/// `depth` `Node`s (pushing `k2_stack` to depth `depth`) before reaching
/// `Leaf fELam`. Calling `fELam` yields `E(...)`, so the composition loop
/// must rebuild all `depth` pending `k2`s into one `Node` chain before the
/// second effect (`EFFECT_TAG2`) can be dispatched.
fn build_entry(depth: usize) -> tidepool_repr::CoreExpr {
    let mut b = TreeBuilder::new();

    let x = b.push(CoreFrame::Var(VarId(0)));
    let val_x = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![x],
    });
    let id_lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val_x,
    });

    let _y = b.push(CoreFrame::Var(VarId(1)));
    let leaf_id_for_fe = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![id_lam],
    });
    let tag2 = b.push(CoreFrame::Lit(Literal::LitWord(EFFECT_TAG2)));
    let request2 = b.push(CoreFrame::Con {
        tag: SECOND_REQUEST_ID,
        fields: vec![],
    });
    let union2 = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag2, request2],
    });
    let e2 = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union2, leaf_id_for_fe],
    });
    let f_e_lam = b.push(CoreFrame::Lam {
        binder: VarId(1),
        body: e2,
    });

    let mut k = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![f_e_lam],
    });
    for _ in 0..depth {
        let leaf_i = b.push(CoreFrame::Con {
            tag: LEAF_ID,
            fields: vec![id_lam],
        });
        k = b.push(CoreFrame::Con {
            tag: NODE_ID,
            fields: vec![k, leaf_i],
        });
    }

    let tag1 = b.push(CoreFrame::Lit(Literal::LitWord(EFFECT_TAG)));
    let req = b.push(CoreFrame::Con {
        tag: FIRST_REQUEST_ID,
        fields: vec![],
    });
    let union1 = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag1, req],
    });
    b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union1, k],
    });
    b.build()
}

/// Dispatches both effect tags with a fixed, checkable answer.
struct TwoTagHandler;
impl DispatchEffect<()> for TwoTagHandler {
    fn dispatch(
        &mut self,
        request: &Value,
        cx: &EffectContext<'_, ()>,
    ) -> Result<Option<Response>, EffectError> {
        match request {
            Value::Con(id, _) if *id == FIRST_REQUEST_ID => cx.respond(0i64).map(Some),
            Value::Con(id, _) if *id == SECOND_REQUEST_ID => cx.respond(99i64).map(Some),
            _ => Ok(None),
        }
    }
}

/// Depth of the left-nested `Node` chain / composition loop within ONE run,
/// how many times that run repeats on the same session machine, and the
/// nursery size they run against. Found EMPIRICALLY (same discipline as
/// `nested_child_response_materialization_gc.rs`'s `CHAIN_DEPTH`/
/// `NURSERY_BYTES`): the exact byte cost of a compiled `Con`/closure
/// allocation is internal to `emit/**`, not a stable, cheap-to-derive
/// constant. Execution is fully deterministic, so a triple confirmed to land
/// in the exhaustion window stays there on every future run against an
/// unchanged compiler; a codegen change that shifts the window makes the
/// final `retry_after == retry_before` assertion fail loudly and name
/// exactly what to retune.
const DEPTH: usize = 20;
const REPEATS: usize = 100;
const NURSERY_BYTES: usize = 1536;

fn run_once() -> Result<(), String> {
    tidepool_codegen::heap_bridge::reset_gc_retry_fired_count();

    let table = table_with_freer_cons();
    let entry = build_entry(DEPTH);
    let mut machine = JitEffectMachine::compile_session(&entry, &table, NURSERY_BYTES)
        .map_err(|e| format!("compile_session: {e}"))?;
    let mut handler = TwoTagHandler;

    let retry_before = tidepool_codegen::heap_bridge::gc_retry_fired_count();
    let mut last = None;
    for i in 0..REPEATS {
        let v = machine
            .run(&table, &mut handler, &())
            .map_err(|e| format!("run {i}/{REPEATS}: {e}"))?;
        last = Some(v);
    }
    let retry_after = tidepool_codegen::heap_bridge::gc_retry_fired_count();

    match last {
        Some(Value::Con(id, ref fields)) if id == I_HASH_ID => match fields.as_slice() {
            [Value::Lit(Literal::LitInt(99))] => {}
            other => return Err(format!("expected I# 99, got I# {other:?}")),
        },
        other => {
            return Err(format!(
                "expected the streamed EFFECT_TAG2 answer 99, got {other:?}"
            ))
        }
    }

    if retry_after == retry_before {
        return Err(format!(
            "gc_retry never fired across {REPEATS} runs (before={retry_before} \
             after={retry_after}) — the composition loop's alloc_con did not actually \
             exhaust the nursery on this run; DEPTH/REPEATS/NURSERY_BYTES need retuning"
        ));
    }
    Ok(())
}

#[test]
#[serial]
fn apply_cont_heap_composition_survives_nursery_exhaustion() {
    run_once().expect("continuation composition must survive nursery exhaustion via GC retry");
}
