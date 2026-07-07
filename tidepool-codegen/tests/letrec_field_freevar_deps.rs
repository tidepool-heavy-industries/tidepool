//! M1 (repo-review-2026-07-06/01-gc-memory-safety.md, Medium findings):
//! LetRec Phase-3b dependency detection matched only direct `Var` Con
//! fields against the deferred-simple-binder set. A non-Var field (e.g.
//! `App g k` where `k` is a Phase-3c "simple" binder — anything in the Rec
//! group that isn't a Lam/Con) was wrongly classified as immediately
//! fillable, so Phase 3b thunkified it right away, before `k` was bound in
//! `ctx.env`. `compute_captures`'s `keep` filter silently drops any free var
//! not yet in scope (no error), so the thunk's capture list is missing `k`;
//! forcing it later hits a Var-miss and calls `unresolved_var_trap`.
//!
//! Fixed by intersecting each field's FREE VARS (not just a direct `Var`
//! child) with the deferred-simple-binder set, so `App g k` is correctly
//! deferred to the same post-step that already handles direct `Var` fields.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::machine_state::MachineState;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;

struct TestResult {
    result_ptr: *const u8,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
    _machine_state: Box<MachineState>,
}

fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, tree, "test_fn", &ExternalEnv::new())
        .expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    let machine_state = Box::new(MachineState::new());
    vmctx.machine_state = machine_state.as_ref() as *const MachineState as *mut MachineState;

    machine_state.set_gc_state(start, nursery.len());
    machine_state.set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    TestResult {
        result_ptr: result as *const u8,
        _nursery: nursery,
        _pipeline: pipeline,
        _machine_state: machine_state,
    }
}

unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT, "expected TAG_LIT");
    *(ptr.add(16) as *const i64)
}

// ---------------------------------------------------------------------------
// let rec g    = \y -> y
//         node = Con_NODE(g k)     -- non-Var field referencing simple binder k
//         k    = 99                -- Phase-3c "simple" binding (not Lam/Con)
// in case node of Con_NODE h -> case h of DEFAULT -> h
//
// `g k` is an `App`, not a direct `Var`, so the old direct-Var-only check
// in Phase 3b never recognized it as depending on the deferred `k` and
// thunkified it immediately — dropping `k` from the thunk's captures.
// Forcing `h` (the inner `case`) then hit a Var-miss on `k` and returned a
// poison value via `unresolved_var_trap` instead of 99.
// ---------------------------------------------------------------------------
#[test]
fn letrec_con_field_app_depends_on_deferred_simple_binder() {
    let g = VarId(1);
    let k = VarId(2);
    let node = VarId(3);
    let y = VarId(4);
    let scrut1 = VarId(5);
    let h = VarId(6);
    let scrut2 = VarId(7);

    const NODE_TAG: DataConId = DataConId(20);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(y),                  // 0: g's body
            CoreFrame::Lam { binder: y, body: 0 }, // 1: g = \y -> y
            CoreFrame::Var(g),                  // 2: ref g
            CoreFrame::Var(k),                  // 3: ref k
            CoreFrame::App { fun: 2, arg: 3 },  // 4: g k  (non-Var Con field)
            CoreFrame::Con {
                tag: NODE_TAG,
                fields: vec![4],
            }, // 5: node = Con_NODE(g k)
            CoreFrame::Lit(Literal::LitInt(99)), // 6: k = 99 (deferred simple binding)
            CoreFrame::Var(node),                // 7: outer case scrutinee
            CoreFrame::Var(h),                   // 8: inner case scrutinee
            CoreFrame::Lit(Literal::LitInt(1)),  // 9: matched-99 marker
            CoreFrame::Lit(Literal::LitInt(0)),  // 10: mismatch marker
            CoreFrame::Case {
                // 11: force h via a Lit dispatch (a Default-only case does NOT
                // force its scrutinee — needs an actual LitAlt to trigger
                // `heap_force`/`emit_lit_dispatch`).
                scrutinee: 8,
                binder: scrut2,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(99)),
                        binders: vec![],
                        body: 9,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 10,
                    },
                ],
            },
            CoreFrame::Case {
                // 12: unwrap node
                scrutinee: 7,
                binder: scrut1,
                alts: vec![Alt {
                    con: AltCon::DataAlt(NODE_TAG),
                    binders: vec![h],
                    body: 11,
                }],
            },
            CoreFrame::LetRec {
                // 13: root
                bindings: vec![(g, 1), (k, 6), (node, 5)],
                body: 12,
            },
        ],
    };

    let result = compile_and_run(&tree);
    let err = host_fns::take_runtime_error();
    assert!(
        err.is_none(),
        "forcing `g k` should not hit a Var-miss on the deferred simple binder `k`: {err:?}"
    );
    unsafe {
        assert_eq!(
            read_lit_int(result.result_ptr),
            1,
            "g k where g = identity and k = 99 should force to 99 and hit the LitAlt(99) branch"
        );
    }
}
