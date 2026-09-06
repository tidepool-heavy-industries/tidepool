//! Closure depth is a property of the program, not of the compiler's native stack.

use tidepool_codegen::emit::{expr::compile_expr, ExternalEnv};
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_repr::{CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind, TreeBuilder, VarId};

fn constructor_spine(depth: usize, op: PrimOpKind) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let mut root = b.push(CoreFrame::PrimOp {
        op,
        args: if op == PrimOpKind::Raise {
            vec![one]
        } else {
            vec![one, one]
        },
    });
    for _ in 0..depth {
        root = b.push(CoreFrame::Con {
            tag: DataConId(0),
            fields: vec![root],
        });
    }
    b.build()
}

fn nested_lambdas(depth: usize) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let mut root = b.push(CoreFrame::Lit(Literal::LitInt(42)));
    for i in 0..depth {
        root = b.push(CoreFrame::Lam {
            binder: VarId(i as u64),
            body: root,
        });
    }
    b.build()
}

fn compile_count(expr: &CoreExpr) -> u64 {
    let mut pipeline = CodegenPipeline::new(&[]).unwrap();
    compile_expr(&mut pipeline, expr, "closure_test", &ExternalEnv::new()).unwrap();
    pipeline.functions_defined()
}

#[test]
fn constructor_structure_does_not_create_thunks() {
    for op in [PrimOpKind::IntAdd, PrimOpKind::Raise] {
        for depth in [1, 32, 256] {
            let count = compile_count(&constructor_spine(depth, op));
            eprintln!("constructor depth={depth} leaf={op:?} functions={count}");
            assert_eq!(count, 2, "only the entry and primitive thunk need code");
        }
    }
}

#[test]
fn nested_constructor_fields_preserve_bottom_until_demanded() {
    use tidepool_codegen::jit_machine::JitEffectMachine;
    use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
    use tidepool_repr::{Alt, AltCon};
    use tidepool_testing::proptest::build_table_for_expr;

    for demand in [false, true] {
        let depth = 32;
        let mut expr = constructor_spine(depth, PrimOpKind::Raise);
        let constructor = expr.nodes.len() - 1;
        let mut push = |node| {
            let idx = expr.nodes.len();
            expr.nodes.push(node);
            idx
        };
        let zero = push(CoreFrame::Lit(Literal::LitInt(0)));
        let leaf = push(CoreFrame::Var(VarId(depth as u64)));
        let mut body = if demand {
            push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![leaf, zero],
            })
        } else {
            push(CoreFrame::Lit(Literal::LitInt(42)))
        };
        for i in (0..depth).rev() {
            let scrutinee = if i == 0 {
                constructor
            } else {
                push(CoreFrame::Var(VarId(i as u64)))
            };
            body = push(CoreFrame::Case {
                scrutinee,
                binder: VarId(100 + i as u64),
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(0)),
                    binders: vec![VarId(i as u64 + 1)],
                    body,
                }],
            });
        }
        let table = build_table_for_expr(&expr);
        let evaluated = eval(&expr, &env_from_datacon_table(&table), &mut VecHeap::new());
        let mut machine = JitEffectMachine::compile(&expr, &table, 512).unwrap();
        let jitted = machine.run_pure();
        if demand {
            assert!(evaluated.is_err());
            assert!(jitted.is_err());
        } else {
            assert!(matches!(evaluated, Ok(Value::Lit(Literal::LitInt(42)))));
            assert!(matches!(jitted, Ok(Value::Lit(Literal::LitInt(42)))));
        }
    }
}

#[test]
fn nested_closures_compile_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let depth = 2048;
            assert_eq!(compile_count(&nested_lambdas(depth)), depth as u64 + 1);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nested_lazy_computations_compile_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut b = TreeBuilder::new();
            let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
            let mut root = one;
            let depth = 1024;
            for _ in 0..depth {
                root = b.push(CoreFrame::PrimOp {
                    op: PrimOpKind::NewArray,
                    args: vec![one, root],
                });
            }
            assert_eq!(compile_count(&b.build()), depth);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn failed_closure_job_prevents_publication_and_reuse() {
    use tidepool_codegen::pipeline::PipelineError;
    let mut b = TreeBuilder::new();
    let invalid = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::SeqOp,
        args: vec![],
    });
    b.push(CoreFrame::Lam {
        binder: VarId(1),
        body: invalid,
    });
    let mut pipeline = CodegenPipeline::new(&[]).unwrap();
    assert!(compile_expr(&mut pipeline, &b.build(), "failed", &ExternalEnv::new()).is_err());
    assert!(pipeline.compilation_failed());
    assert_eq!(
        pipeline.functions_defined(),
        1,
        "the entry was emitted before its failed job"
    );
    // Cleanup and retained diagnostics must never resolve the failed closure.
    let _ = pipeline.build_lambda_registry();
    assert!(matches!(
        pipeline.finalize(),
        Err(PipelineError::IncompleteCompilation)
    ));
    assert!(compile_expr(
        &mut pipeline,
        &nested_lambdas(1),
        "later",
        &ExternalEnv::new()
    )
    .is_err());
    assert_eq!(pipeline.functions_defined(), 1);
    // A separately owned pipeline remains usable.
    assert_eq!(compile_count(&nested_lambdas(1)), 2);
}

#[test]
fn cranelift_compilation_grows_a_depleted_stack() {
    use cranelift_codegen::ir::{types, InstBuilder};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use std::hint::black_box;

    #[inline(never)]
    fn consume_stack(action: &mut dyn FnMut()) {
        let pad = [0_u8; 4096];
        black_box(&pad);
        if stacker::remaining_stack().unwrap() < 24 * 1024 {
            action();
        } else {
            consume_stack(action);
        }
        black_box(&pad);
    }

    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut pipeline = CodegenPipeline::new(&[]).unwrap();
            let id = pipeline.declare_function("depleted").unwrap();
            let mut ctx = cranelift_codegen::Context::new();
            ctx.func.signature = pipeline.make_func_signature();
            let mut fb = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);
            let result = builder.ins().iconst(types::I64, 42);
            builder.ins().return_(&[result]);
            builder.finalize();
            consume_stack(&mut || pipeline.define_function(id, &mut ctx).unwrap());
            pipeline.finalize().unwrap();
            assert_eq!(pipeline.functions_defined(), 1);
        })
        .unwrap()
        .join()
        .unwrap();
}
