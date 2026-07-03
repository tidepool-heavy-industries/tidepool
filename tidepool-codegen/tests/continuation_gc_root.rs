//! Regression guard: the effect-drive loops must GC-root the `continuation`
//! heap pointer across response materialization.
//!
//! Between `Yield::Request` and `machine.resume(continuation, …)` the JIT
//! stack is unwound, so the frame walker sees no JIT frames — the only live
//! roots are RUST_ROOTS / PERSISTENT_ROOTS / the vmctx tail slots. Response
//! materialization (`materialize_cons_list` → `build_cons_cells` →
//! `host_alloc_gc`) and request forcing can trigger a collection; an UNROOTED
//! continuation tree is then not evacuated, from-space is freed, and resume
//! reads freed memory (UB: garbage tag, SIGSEGV, or silent corruption).
//!
//! This test forces that exact window deterministically: a one-effect program
//! whose handler responds with a 3000-cell list, materialized EAGERLY
//! (`TIDEPOOL_LAZY_RESULTS=0`) into a 16 KiB nursery — the materialization
//! must collect (repeatedly, with heap doubling) while the continuation is
//! live, and the test ASSERTS the collections fired (gc_trigger_call_count).
//!
//! Honesty note: the unrooted-continuation failure is a silent use-after-free
//! — glibc does not scrub freed pages, so on the un-fixed code this test's
//! assertions can still pass by luck (verified: the negative control passed).
//! What the test pins is the INVARIANT PATH: a collection provably fires
//! inside the request window and the run must complete correctly — with the
//! rooting fix that is correct by construction (the GC rewrites the rooted
//! slot) rather than by allocator accident. The fix itself is mandated by
//! `host_alloc_gc`'s documented contract ("any heap pointers the CALLER holds
//! across this call must be RUST_ROOTS-registered").

use tidepool_codegen::effect_machine::EffContKind;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};
use tidepool_effect::dispatch::DispatchEffect;

const CONS_ID: u64 = 5000;
const NIL_ID: u64 = 5001;
const LIST_LEN: usize = 3000;

fn test_table() -> DataConTable {
    let mut table = DataConTable::new();
    // Freer-simple tags required by `JitEffectMachine::compile`.
    for (i, kind) in EffContKind::ALL.iter().enumerate() {
        table.insert(tidepool_repr::datacon::DataCon {
            id: DataConId(1000 + i as u64),
            name: kind.name().to_string(),
            tag: (1000 + i) as u32,
            rep_arity: if matches!(kind, EffContKind::Node | EffContKind::Union) {
                2
            } else {
                1
            },
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    // List constructors for the handler's response.
    table.insert(tidepool_repr::datacon::DataCon {
        id: DataConId(CONS_ID),
        name: ":".to_string(),
        tag: CONS_ID as u32,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
    });
    table.insert(tidepool_repr::datacon::DataCon {
        id: DataConId(NIL_ID),
        name: "[]".to_string(),
        tag: NIL_ID as u32,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    table
}

fn con_id_for(table: &DataConTable, kind: EffContKind) -> DataConId {
    let idx = EffContKind::ALL
        .iter()
        .position(|k| k.name() == kind.name())
        .unwrap();
    let _ = table; // ids are assigned positionally above
    DataConId(1000 + idx as u64)
}

/// `E (Union (W# 0) (I# 42)) (Leaf (\v -> Val v))` — a single effect whose
/// continuation is the identity: the drive loop dispatches tag 0 with request
/// 42, and the effect response becomes the program's final value.
fn build_one_effect_program(table: &DataConTable) -> CoreExpr {
    let e_id = con_id_for(table, EffContKind::E);
    let union_id = con_id_for(table, EffContKind::Union);
    let leaf_id = con_id_for(table, EffContKind::Leaf);
    let val_id = con_id_for(table, EffContKind::Val);

    let mut bld = TreeBuilder::new();
    let tag_word = bld.push(CoreFrame::Lit(Literal::LitWord(0)));
    let request = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    let union = bld.push(CoreFrame::Con {
        tag: union_id,
        fields: vec![tag_word, request],
    });
    // \v -> Val v
    let var_v = bld.push(CoreFrame::Var(VarId(0)));
    let val_v = bld.push(CoreFrame::Con {
        tag: val_id,
        fields: vec![var_v],
    });
    let ident = bld.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val_v,
    });
    let leaf = bld.push(CoreFrame::Con {
        tag: leaf_id,
        fields: vec![ident],
    });
    bld.push(CoreFrame::Con {
        tag: e_id,
        fields: vec![union, leaf],
    });
    bld.build()
}

/// Responds to any effect with a LIST_LEN-cell cons list of ints. Built
/// iteratively (nil outward) so construction itself is stack-safe.
struct ListResponder;

impl DispatchEffect<()> for ListResponder {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        let mut acc = Value::Con(DataConId(NIL_ID), vec![]);
        for i in (0..LIST_LEN).rev() {
            acc = Value::Con(
                DataConId(CONS_ID),
                vec![Value::Lit(Literal::LitInt(i as i64)), acc],
            );
        }
        Ok(Response::Complete(acc))
    }
}

/// Count the cells of a cons-list `Value` and check the first/last payloads.
fn assert_full_list(v: &Value) {
    let mut len = 0usize;
    let mut cur = v;
    let mut first: Option<i64> = None;
    let mut last: Option<i64> = None;
    loop {
        match cur {
            Value::Con(id, fields) if id.0 == CONS_ID && fields.len() == 2 => {
                if let Value::Lit(Literal::LitInt(n)) = &fields[0] {
                    if first.is_none() {
                        first = Some(*n);
                    }
                    last = Some(*n);
                } else {
                    panic!("non-int list head: {:?}", fields[0]);
                }
                len += 1;
                cur = &fields[1];
            }
            Value::Con(id, fields) if id.0 == NIL_ID && fields.is_empty() => break,
            other => panic!("unexpected list shape at cell {len}: {other:?}"),
        }
    }
    assert_eq!(len, LIST_LEN, "list length after GC-through-materialization");
    assert_eq!(first, Some(0));
    assert_eq!(last, Some((LIST_LEN - 1) as i64));
}

#[test]
fn continuation_survives_gc_during_response_materialization() {
    // Kill-switch: force EAGER in-arm materialization (materialize_cons_list →
    // host_alloc_gc), so the collection is guaranteed to fire while the
    // continuation is live. Env is per-process; this file has one test.
    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "0");

    let table = test_table();
    let expr = build_one_effect_program(&table);

    // 16 KiB nursery: LIST_LEN cells (~40 B each) require multiple
    // collections + heap doublings during materialization.
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 14)
        .expect("compile one-effect program");
    let mut handler = ListResponder;

    tidepool_codegen::host_fns::reset_test_counters();
    let result = machine
        .run(&table, &mut handler, &())
        .expect("run must complete: the rooted continuation survives GC");
    let collections = tidepool_codegen::host_fns::gc_trigger_call_count();
    assert!(
        collections > 0,
        "the test premise requires GC to fire inside the request window \
         (materialize_cons_list into an undersized nursery); it fired 0 times \
         — the fixture no longer exercises the exposure"
    );
    assert_full_list(&result);

    std::env::remove_var("TIDEPOOL_LAZY_RESULTS");
}
