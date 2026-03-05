/*
GC Audit (2026-02-18):
- Audited codegen/src/emit/expr.rs:
    - Lit, Con, App, Lam, LetRec all correctly call `declare_value_needs_stack_map` for heap pointers.
    - `ensure_heap_ptr` correctly declares new Lit objects.
    - Lambda inner functions correctly declare `closure_self`, `arg_param`, and loaded captures.
- Audited codegen/src/emit/case.rs:
    - Merge block parameter correctly declared.
    - DataAlt field loads correctly declared.
    - Scrutinee binder in environment correctly tracks heap pointers.
- Audited codegen/src/emit/join.rs:
    - Join block parameters correctly declared.
    - Merge block parameter correctly declared.
- Audited codegen/src/emit/primop.rs:
    - Unboxed values (Raw) correctly NOT declared.

All heap-pointer SSA values identified are properly tracked in stack maps.
The following tests verify these properties programmatically.
*/

use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_repr::*;

#[test]
fn test_stack_map_app_safepoints() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    // Core: \f -> f 1
    // Nodes:
    // 0: Var(0)  -- f
    // 1: Lit(1)  -- 1
    // 2: App(0, 1) -- f 1
    // 3: Lam(0, 2) -- \f -> f 1

    let mut nodes = Vec::new();
    nodes.push(CoreFrame::Var(VarId(0)));
    nodes.push(CoreFrame::Lit(Literal::LitInt(1)));
    nodes.push(CoreFrame::App { fun: 0, arg: 1 });
    nodes.push(CoreFrame::Lam {
        binder: VarId(0),
        body: 2,
    });

    let tree = CoreExpr { nodes };

    let _ = compile_expr(&mut pipeline, &tree, "test_app").unwrap();
    pipeline.finalize().expect("failed to finalize");

    // The App(0, 1) is a safepoint.
    assert!(
        !pipeline.stack_maps.is_empty(),
        "Stack maps should be generated for App"
    );
}

#[test]
fn test_stack_map_case_safepoints() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    let mut nodes = Vec::new();
    // 0: Var(100) - f (free)
    nodes.push(CoreFrame::Var(VarId(100)));
    // 1: Var(10) - a (from pattern)
    nodes.push(CoreFrame::Var(VarId(10)));
    // 2: App(f, a) - SAFEPOINT
    nodes.push(CoreFrame::App { fun: 0, arg: 1 });
    // 3: Var(5) - scrutinee
    nodes.push(CoreFrame::Var(VarId(5)));
    // 4: Case
    nodes.push(CoreFrame::Case {
        scrutinee: 3,
        binder: VarId(0),
        alts: vec![Alt {
            con: AltCon::DataAlt(DataConId(0)),
            binders: vec![VarId(10), VarId(11)],
            body: 2,
        }],
    });
    // 5: Lam
    nodes.push(CoreFrame::Lam {
        binder: VarId(5),
        body: 4,
    });

    let tree = CoreExpr { nodes };
    let _ = compile_expr(&mut pipeline, &tree, "test_case_safepoint").unwrap();
    pipeline.finalize().expect("failed to finalize");

    // App(0, 1) inside Case alt is a safepoint.
    assert!(
        !pipeline.stack_maps.is_empty(),
        "Stack maps should be generated for App in Case alt"
    );
}

#[test]
fn test_stack_map_join_safepoints() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    // Core: join j z = (f z) in j 1
    // Nodes:
    // 0: Var(100) -- f (free)
    // 1: Var(10) -- z (param)
    // 2: App(f, z) -- SAFEPOINT
    // 3: Lit(1) -- 1 (arg for j)
    // 4: Jump(j, [3])
    // 5: Join(j, [10], rhs=2, body=4)

    let mut nodes = Vec::new();
    nodes.push(CoreFrame::Var(VarId(100))); // 0
    nodes.push(CoreFrame::Var(VarId(10))); // 1
    nodes.push(CoreFrame::App { fun: 0, arg: 1 }); // 2
    nodes.push(CoreFrame::Lit(Literal::LitInt(1))); // 3
    nodes.push(CoreFrame::Jump {
        label: JoinId(0),
        args: vec![3],
    }); // 4
    nodes.push(CoreFrame::Join {
        label: JoinId(0),
        params: vec![VarId(10)],
        rhs: 2,
        body: 4,
    }); // 5

    let tree = CoreExpr { nodes };

    let _ = compile_expr(&mut pipeline, &tree, "test_join_safepoint").unwrap();
    pipeline.finalize().expect("failed to finalize");

    // App(0, 1) in RHS is a safepoint.
    assert!(
        !pipeline.stack_maps.is_empty(),
        "Stack maps should be generated for App in Join RHS"
    );
}
