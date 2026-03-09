use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;

struct TestResult {
    result_ptr: *const u8,
    vmctx: VMContext,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

impl TestResult {
    /// Force a heap pointer (resolve thunks to WHNF).
    unsafe fn force(&mut self, ptr: *const u8) -> *const u8 {
        host_fns::heap_force(&mut self.vmctx, ptr as *mut u8) as *const u8
    }
}

/// Helper: set up pipeline + nursery, compile expr, call it, return result ptr.
fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536]; // 64KB nursery
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    TestResult {
        result_ptr: result as *const u8,
        vmctx,
        _nursery: nursery,
        _pipeline: pipeline,
    }
}

/// Helper: read i64 value from a LitObject.
unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    *(ptr.add(16) as *const i64)
}

unsafe fn read_lit_double(ptr: *const u8) -> f64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    f64::from_bits(*(ptr.add(16) as *const u64))
}

unsafe fn read_lit_float(ptr: *const u8) -> f32 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    f32::from_bits(*(ptr.add(16) as *const u32))
}

/// Helper: read con_tag from a ConObject.
unsafe fn read_con_tag(ptr: *const u8) -> u64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_CON);
    *(ptr.add(8) as *const u64)
}

/// Helper: read field i from a ConObject.
unsafe fn read_con_field(ptr: *const u8, i: usize) -> *const u8 {
    *(ptr.add(24 + 8 * i) as *const *const u8)
}

#[test]
fn test_emit_lit_int() {
    let tree = RecursiveTree {
        nodes: vec![CoreFrame::Lit(Literal::LitInt(42))],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_primop_int_add() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2 (root)
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 3);
    }
}

#[test]
fn test_emit_primop_int_and() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(0xFF)),
            CoreFrame::Lit(Literal::LitInt(0x0F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAnd,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0x0F);
    }
}

#[test]
fn test_emit_primop_int_or() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(0xF0)),
            CoreFrame::Lit(Literal::LitInt(0x0F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntOr,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0xFF);
    }
}

#[test]
fn test_emit_primop_int_xor() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(0xFF)),
            CoreFrame::Lit(Literal::LitInt(0x0F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntXor,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0xF0);
    }
}

#[test]
fn test_emit_primop_int_not() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(0)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntNot,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -1);
    }
}

#[test]
fn test_emit_primop_int_shl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(8)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntShl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 256);
    }
}

#[test]
fn test_emit_primop_int_shra() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(-16)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntShra,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -4);
    }
}

#[test]
fn test_emit_primop_word_and() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0xFF)),
            CoreFrame::Lit(Literal::LitWord(0x0F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordAnd,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0x0F);
    }
}

#[test]
fn test_emit_primop_word_or() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0xF0)),
            CoreFrame::Lit(Literal::LitWord(0x0F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordOr,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0xFF);
    }
}

#[test]
fn test_emit_primop_word_shl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(1)),
            CoreFrame::Lit(Literal::LitInt(32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordShl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1i64 << 32);
    }
}

#[test]
fn test_emit_primop_word_shrl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(256)),
            CoreFrame::Lit(Literal::LitInt(4)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordShrl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 16);
    }
}

#[test]
fn test_emit_primop_narrow_narrow8int() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(257)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Narrow8Int,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_narrow_narrow16int() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(65537)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Narrow16Int,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_narrow_narrow8word() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0x1FF)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Narrow8Word,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0xFF);
    }
}

#[test]
fn test_emit_primop_word_add_word_c_val() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(10)),
            CoreFrame::Lit(Literal::LitWord(20)),
            CoreFrame::PrimOp {
                op: PrimOpKind::AddWordCVal,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 30);
    }
}

#[test]
fn test_emit_primop_word_sub_word_c_val() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(30)),
            CoreFrame::Lit(Literal::LitWord(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::SubWordCVal,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 20);
    }
}

#[test]
fn test_emit_primop_word_add_word_c_carry() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(u64::MAX)),
            CoreFrame::Lit(Literal::LitWord(1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::AddWordCCarry,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_word_sub_word_c_carry() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0)),
            CoreFrame::Lit(Literal::LitWord(1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::SubWordCCarry,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_word_quot() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(100)),
            CoreFrame::Lit(Literal::LitWord(7)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordQuot,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 14);
    }
}

#[test]
fn test_emit_primop_word_rem() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(100)),
            CoreFrame::Lit(Literal::LitWord(7)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordRem,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 2);
    }
}

#[test]
fn test_emit_primop_char_ord() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitChar('A')),
            CoreFrame::PrimOp {
                op: PrimOpKind::Ord,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 65);
    }
}

#[test]
fn test_emit_primop_char_chr() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(65)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Chr,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 65);
    }
}

#[test]
fn test_emit_primop_char_eq() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitChar('a')),
            CoreFrame::Lit(Literal::LitChar('a')),
            CoreFrame::PrimOp {
                op: PrimOpKind::CharEq,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_char_lt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitChar('a')),
            CoreFrame::Lit(Literal::LitChar('b')),
            CoreFrame::PrimOp {
                op: PrimOpKind::CharLt,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_con_fields() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(20)), // 1
            CoreFrame::Con {
                tag: DataConId(5),
                fields: vec![0, 1],
            }, // 2 (root)
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        assert_eq!(read_con_tag(result.result_ptr), 5);
        let f0 = read_con_field(result.result_ptr, 0);
        let f1 = read_con_field(result.result_ptr, 1);
        assert_eq!(read_lit_int(f0), 10);
        assert_eq!(read_lit_int(f1), 20);
    }
}

#[test]
fn test_emit_identity_lambda() {
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),   // 0: arg
            CoreFrame::Var(x),                     // 1: body (x)
            CoreFrame::Lam { binder: x, body: 1 }, // 2: λx.x
            CoreFrame::App { fun: 2, arg: 0 },     // 3: (λx.x) 42 (root)
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_closure_capture() {
    let x = VarId(1);
    let y = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0: y = 1
            CoreFrame::Var(x),                  // 1: x
            CoreFrame::Var(y),                  // 2: y
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            }, // 3: x + y
            CoreFrame::Lam { binder: x, body: 3 }, // 4: λx. x + y
            CoreFrame::Lit(Literal::LitInt(2)), // 5: arg = 2
            CoreFrame::App { fun: 4, arg: 5 },  // 6: (λx. x+y) 2
            CoreFrame::LetNonRec {
                binder: y,
                rhs: 0,
                body: 6,
            }, // 7: let y=1 in ... (root)
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 3);
    }
}

#[test]
fn test_emit_let_non_rec() {
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(x),                   // 1
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2 (root)
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_primop_int_sub() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 7);
    }
}

#[test]
fn test_emit_primop_int_mul() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(6)),
            CoreFrame::Lit(Literal::LitInt(7)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntMul,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_primop_int_negate() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntNegate,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -42);
    }
}

#[test]
fn test_emit_primop_int_comparisons() {
    fn run_cmp(op: PrimOpKind, a: i64, b: i64) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(a)),
                CoreFrame::Lit(Literal::LitInt(b)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }

    assert_eq!(run_cmp(PrimOpKind::IntEq, 5, 5), 1);
    assert_eq!(run_cmp(PrimOpKind::IntEq, 5, 6), 0);
    assert_eq!(run_cmp(PrimOpKind::IntNe, 5, 6), 1);
    assert_eq!(run_cmp(PrimOpKind::IntNe, 5, 5), 0);
    assert_eq!(run_cmp(PrimOpKind::IntLt, 3, 5), 1);
    assert_eq!(run_cmp(PrimOpKind::IntLt, 5, 3), 0);
    assert_eq!(run_cmp(PrimOpKind::IntLe, 5, 5), 1);
    assert_eq!(run_cmp(PrimOpKind::IntLe, 6, 5), 0);
    assert_eq!(run_cmp(PrimOpKind::IntGt, 5, 3), 1);
    assert_eq!(run_cmp(PrimOpKind::IntGt, 3, 5), 0);
    assert_eq!(run_cmp(PrimOpKind::IntGe, 5, 5), 1);
    assert_eq!(run_cmp(PrimOpKind::IntGe, 4, 5), 0);
}

#[test]
fn test_emit_primop_word_ops() {
    fn run_word(op: PrimOpKind, a: u64, b: u64) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(a)),
                CoreFrame::Lit(Literal::LitWord(b)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }

    assert_eq!(run_word(PrimOpKind::WordAdd, 10, 20), 30);
    assert_eq!(run_word(PrimOpKind::WordSub, 20, 10), 10);
    assert_eq!(run_word(PrimOpKind::WordMul, 6, 7), 42);
}

#[test]
fn test_emit_word_boxing() {
    // PrimOp(WordAdd, [LitWord(1), LitWord(2)]) -> should be boxed as LitWord
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(1)),
            CoreFrame::Lit(Literal::LitWord(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordAdd,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        // Offset 8 is lit_tag
        assert_eq!(
            *result.result_ptr.add(8),
            tidepool_codegen::emit::LIT_TAG_WORD as u8
        );
        assert_eq!(read_lit_int(result.result_ptr), 3);
    }
}

#[test]
fn test_emit_let_rec() {
    // let rec f = λn. if n == 0 then 1 else n * f (n-1) in f 5
    // Simplified: let rec f = λx. x in f 42
    let f = VarId(1);
    let x = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(x),                     // 0: x
            CoreFrame::Lam { binder: x, body: 0 }, // 1: λx.x
            CoreFrame::Lit(Literal::LitInt(42)),   // 2: arg 42
            CoreFrame::Var(f),                     // 3: f
            CoreFrame::App { fun: 3, arg: 2 },     // 4: f 42
            CoreFrame::LetRec {
                bindings: vec![(f, 1)],
                body: 4,
            }, // 5: let rec f = ... (root)
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_let_rec_factorial_ish() {
    // let rec f = λn. if n == 0 then 1 else n * f (n-1)
    // Since I don't have Case/If yet, I'll test a simple mutually recursive pair or just recursion.
    // let rec f = λn. n in f 10
    let f = VarId(1);
    let n = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(n),                     // 0: n
            CoreFrame::Lam { binder: n, body: 0 }, // 1: f = λn. n
            CoreFrame::Lit(Literal::LitInt(10)),   // 2: 10
            CoreFrame::Var(f),                     // 3: f
            CoreFrame::App { fun: 3, arg: 2 },     // 4: f 10
            CoreFrame::LetRec {
                bindings: vec![(f, 1)],
                body: 4,
            }, // 5: root
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 10);
    }
}

#[test]
fn test_emit_let_rec_mutual() {
    // let rec f = λx. x
    //         g = λy. f y
    // in g 42
    let f_id = VarId(1);
    let g_id = VarId(2);
    let x_id = VarId(3);
    let y_id = VarId(4);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(x_id), // 0: x
            CoreFrame::Lam {
                binder: x_id,
                body: 0,
            }, // 1: f = λx.x
            CoreFrame::Var(f_id), // 2: f
            CoreFrame::Var(y_id), // 3: y
            CoreFrame::App { fun: 2, arg: 3 }, // 4: f y
            CoreFrame::Lam {
                binder: y_id,
                body: 4,
            }, // 5: g = λy. f y
            CoreFrame::Lit(Literal::LitInt(42)), // 6: 42
            CoreFrame::Var(g_id), // 7: g
            CoreFrame::App { fun: 7, arg: 6 }, // 8: g 42
            CoreFrame::LetRec {
                bindings: vec![(f_id, 1), (g_id, 5)],
                body: 8,
            }, // 9: root
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_unique_lambda_names() {
    // Compile two different expressions that both have lambdas using the same pipeline.
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    let x = VarId(1);
    // λx.x
    let tree1 = RecursiveTree {
        nodes: vec![CoreFrame::Var(x), CoreFrame::Lam { binder: x, body: 0 }],
    };

    // λx. x + 1
    let tree2 = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(x),
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            },
            CoreFrame::Lam { binder: x, body: 2 },
        ],
    };

    // This should NOT panic due to "duplicate function name"

    compile_expr(&mut pipeline, &tree1, "f1").expect("First compilation failed");

    compile_expr(&mut pipeline, &tree2, "f2").expect("Second compilation failed");
}

#[test]
fn test_emit_runtime_error_div_zero() {
    // A Var with tag 'E' (0x45) and kind 0 returns a lazy poison closure.
    // Error flag is NOT set until the closure is actually called.
    let div_zero_var = VarId(0x4500000000000000); // tag 'E', kind 0 = divZero
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(div_zero_var), // 0 (root)
        ],
    };
    let result = compile_and_run(&tree);
    assert!(!result.result_ptr.is_null()); // lazy poison closure, not null
    assert_eq!(result.result_ptr, host_fns::error_poison_ptr_lazy(0));
    // Error flag is NOT set — lazy poison defers until call
    let err = host_fns::take_runtime_error();
    assert!(err.is_none());
}

#[test]
fn test_emit_runtime_error_overflow() {
    let overflow_var = VarId(0x4500000000000001); // tag 'E', kind 1 = overflow
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(overflow_var), // 0 (root)
        ],
    };
    let result = compile_and_run(&tree);
    assert!(!result.result_ptr.is_null()); // lazy poison closure, not null
    assert_eq!(result.result_ptr, host_fns::error_poison_ptr_lazy(1));
    // Error flag is NOT set — lazy poison defers until call
    let err = host_fns::take_runtime_error();
    assert!(err.is_none());
}

#[test]
fn test_emit_int_quot() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::Lit(Literal::LitInt(6)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntQuot,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 7);
    }
}

#[test]
fn test_emit_int_rem() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::Lit(Literal::LitInt(5)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntRem,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 2);
    }
}

#[test]
fn test_emit_primop_bytearray_basic() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(16)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::SizeofByteArray,
                args: vec![1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 16);
    }
}

#[test]
fn test_emit_primop_bytearray_read_write() {
    let ba = VarId(1);
    let dummy = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::Var(ba),                  // 2
            CoreFrame::Lit(Literal::LitInt(3)),  // 3
            CoreFrame::Lit(Literal::LitWord(42)), // 4
            CoreFrame::PrimOp {
                op: PrimOpKind::WriteWord8Array,
                args: vec![2, 3, 4],
            }, // 5
            CoreFrame::Var(ba),                  // 6
            CoreFrame::Lit(Literal::LitInt(3)),  // 7
            CoreFrame::PrimOp {
                op: PrimOpKind::ReadWord8Array,
                args: vec![6, 7],
            }, // 8
            CoreFrame::Var(dummy),               // 9
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![8, 9],
            }, // 10: read + dummy (0)
            CoreFrame::LetNonRec {
                binder: dummy,
                rhs: 5,
                body: 10,
            }, // 11
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 1,
                body: 11,
            }, // 12
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_primop_bytearray_word_read_write() {
    let ba = VarId(1);
    let dummy = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            },
            CoreFrame::Var(ba),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::Lit(Literal::LitWord(0x1122334455667788)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WriteWordArray,
                args: vec![2, 3, 4],
            },
            CoreFrame::Var(ba),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IndexWordArray,
                args: vec![6, 7],
            },
            CoreFrame::Var(dummy),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordAdd,
                args: vec![8, 9],
            },
            CoreFrame::LetNonRec {
                binder: dummy,
                rhs: 5,
                body: 10,
            },
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 1,
                body: 11,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr) as u64, 0x1122334455667788);
    }
}

#[test]
fn test_emit_primop_bytearray_copy() {
    let ba1 = VarId(1);
    let ba2 = VarId(2);
    let dummy1 = VarId(3);
    let dummy2 = VarId(4);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 2
            CoreFrame::Var(ba1),
            CoreFrame::Lit(Literal::LitInt(0)),
            CoreFrame::Lit(Literal::LitWord(123)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WriteWord8Array,
                args: vec![3, 4, 5],
            }, // 6
            CoreFrame::Var(ba1),                // 7
            CoreFrame::Lit(Literal::LitInt(0)), // 8
            CoreFrame::Var(ba2),                // 9
            CoreFrame::Lit(Literal::LitInt(5)), // 10
            CoreFrame::Lit(Literal::LitInt(1)), // 11
            CoreFrame::PrimOp {
                op: PrimOpKind::CopyByteArray,
                args: vec![7, 8, 9, 10, 11],
            }, // 12
            CoreFrame::Var(ba2),                // 13
            CoreFrame::Lit(Literal::LitInt(5)), // 14
            CoreFrame::PrimOp {
                op: PrimOpKind::ReadWord8Array,
                args: vec![13, 14],
            }, // 15
            CoreFrame::Var(dummy1),             // 16
            CoreFrame::Var(dummy2),             // 17
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![16, 17],
            }, // 18
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![15, 18],
            }, // 19
            CoreFrame::LetNonRec {
                binder: dummy2,
                rhs: 12,
                body: 19,
            },
            CoreFrame::LetNonRec {
                binder: dummy1,
                rhs: 6,
                body: 20,
            },
            CoreFrame::LetNonRec {
                binder: ba2,
                rhs: 2,
                body: 21,
            },
            CoreFrame::LetNonRec {
                binder: ba1,
                rhs: 1,
                body: 22,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 123);
    }
}

#[test]
fn test_emit_primop_bytearray_freeze() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::UnsafeFreezeByteArray,
                args: vec![1],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::SizeofByteArray,
                args: vec![2],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 10);
    }
}

#[test]
fn test_emit_primop_plus_addr() {
    // We use a LitString to get a valid Addr#
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitString(b"ABC".to_vec())),
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::PlusAddr,
                args: vec![0, 1],
            },
            CoreFrame::Lit(Literal::LitInt(0)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IndexCharOffAddr,
                args: vec![2, 3],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 'B' as i64);
    }
}

#[test]
fn test_emit_primop_char_roundtrip() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(65)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Chr,
                args: vec![0],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::Ord,
                args: vec![1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 65);
    }
}

#[test]
fn test_emit_primop_int_comparisons_extra() {
    fn run_cmp(op: PrimOpKind, a: i64, b: i64) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(a)),
                CoreFrame::Lit(Literal::LitInt(b)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }
    assert_eq!(run_cmp(PrimOpKind::IntLt, -5, -3), 1);
    assert_eq!(run_cmp(PrimOpKind::IntGt, -3, -5), 1);
    assert_eq!(run_cmp(PrimOpKind::IntLe, -5, -5), 1);
    assert_eq!(run_cmp(PrimOpKind::IntGe, -3, -5), 1);
}

#[test]
fn test_emit_primop_word_comparisons() {
    fn run_cmp(op: PrimOpKind, a: u64, b: u64) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(a)),
                CoreFrame::Lit(Literal::LitWord(b)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }
    assert_eq!(run_cmp(PrimOpKind::WordLt, 10, 20), 1);
    assert_eq!(run_cmp(PrimOpKind::WordGt, 20, 10), 1);
    assert_eq!(run_cmp(PrimOpKind::WordLe, 10, 10), 1);
    assert_eq!(run_cmp(PrimOpKind::WordGe, 10, 5), 1);
}

#[test]
fn test_emit_primop_int_quot_rem_neg() {
    let tree_quot = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(-10)),
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntQuot,
                args: vec![0, 1],
            },
        ],
    };
    let res_quot = compile_and_run(&tree_quot);
    unsafe {
        assert_eq!(read_lit_int(res_quot.result_ptr), -3);
    }

    let tree_rem = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(-10)),
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntRem,
                args: vec![0, 1],
            },
        ],
    };
    let res_rem = compile_and_run(&tree_rem);
    unsafe {
        assert_eq!(read_lit_int(res_rem.result_ptr), -1);
    }
}

#[test]
fn test_emit_primop_word_quot_rem() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(10)),
            CoreFrame::Lit(Literal::LitWord(3)),
            CoreFrame::PrimOp {
                op: PrimOpKind::QuotRemWordVal,
                args: vec![0, 1],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::QuotRemWordRem,
                args: vec![0, 1],
            },
            CoreFrame::Con {
                tag: DataConId(0),
                fields: vec![2, 3],
            },
        ],
    };
    let mut result = compile_and_run(&tree);
    unsafe {
        let f0 = result.force(read_con_field(result.result_ptr, 0));
        let f1 = result.force(read_con_field(result.result_ptr, 1));
        assert_eq!(read_lit_int(f0), 3);
        assert_eq!(read_lit_int(f1), 1);
    }
}

#[test]
fn test_emit_primop_word8_ops() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(250)),
            CoreFrame::Lit(Literal::LitWord(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word8Add,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 4);
    } // (250 + 10) % 256 = 4

    let tree_sub = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(5)),
            CoreFrame::Lit(Literal::LitWord(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word8Sub,
                args: vec![0, 1],
            },
        ],
    };
    let result_sub = compile_and_run(&tree_sub);
    unsafe {
        assert_eq!(read_lit_int(result_sub.result_ptr), 251);
    } // (5 - 10) % 256 = 251
}

#[test]
fn test_emit_primop_clz8() {
    let cases = vec![(0x01, 7), (0x80, 0), (0x00, 8), (0xFF, 0), (0x40, 1)];
    for (input, expected) in cases {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(input)),
                CoreFrame::PrimOp {
                    op: PrimOpKind::Clz8,
                    args: vec![0],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe {
            // Note: clz8# returns Word#, but we read as int for convenience
            assert_eq!(read_lit_int(result.result_ptr), expected as i64);
        }
    }
}

#[test]
fn test_emit_primop_narrowing() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(0x11223344)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Narrow16Int,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0x3344);
    }
}

#[test]
fn test_emit_primop_add_int_c() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(i64::MAX)),
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::AddIntCCarry,
                args: vec![0, 1],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::AddIntCVal,
                args: vec![0, 1],
            },
            CoreFrame::Con {
                tag: DataConId(0),
                fields: vec![2, 3],
            },
        ],
    };
    let mut result = compile_and_run(&tree);
    unsafe {
        let f0 = result.force(read_con_field(result.result_ptr, 0)); // carry
        let f1 = result.force(read_con_field(result.result_ptr, 1)); // val
        assert_eq!(read_lit_int(f0), 1);
        assert_eq!(read_lit_int(f1), i64::MIN);
    }
}

#[test]
fn test_emit_primop_double_add() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(1.5))),
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(2.5))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleAdd,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), 4.0);
    }
}

#[test]
fn test_emit_primop_double_sub() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(5.0))),
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(3.0))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleSub,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), 2.0);
    }
}

#[test]
fn test_emit_primop_double_mul() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(3.0))),
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(4.0))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleMul,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), 12.0);
    }
}

#[test]
fn test_emit_primop_double_div() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(10.0))),
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(4.0))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleDiv,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), 2.5);
    }
}

#[test]
fn test_emit_primop_double_negate() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(3.14))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleNegate,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), -3.14);
    }
}

#[test]
fn test_emit_primop_double_eq() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(1.0))),
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(1.0))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleEq,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_double_lt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(1.0))),
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(2.0))),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleLt,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_float_add() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(1.5) as u64)),
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(2.5) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatAdd,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), 4.0);
    }
}

#[test]
fn test_emit_primop_float_mul() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(3.0) as u64)),
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(4.0) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatMul,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), 12.0);
    }
}

#[test]
fn test_emit_primop_float_negate() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(2.0) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatNegate,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), -2.0);
    }
}

#[test]
fn test_emit_primop_float_lt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(1.0) as u64)),
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(2.0) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatLt,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_int2double_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int2Double,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), 42.0);
    }
}

#[test]
fn test_emit_double2int_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(3.7))),
            CoreFrame::PrimOp {
                op: PrimOpKind::Double2Int,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 3);
    }
}

#[test]
fn test_emit_int2float_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int2Float,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), 42.0);
    }
}

#[test]
fn test_emit_float2double_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(1.5) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Float2Double,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_double(result.result_ptr), 1.5);
    }
}

#[test]
fn test_emit_primop_int64_add() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(100)),
            CoreFrame::Lit(Literal::LitInt(200)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Add,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 300);
    }
}

#[test]
fn test_emit_primop_int64_sub() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(100)),
            CoreFrame::Lit(Literal::LitInt(200)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Sub,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -100);
    }
}

#[test]
fn test_emit_primop_int64_mul() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(7)),
            CoreFrame::Lit(Literal::LitInt(8)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Mul,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 56);
    }
}

#[test]
fn test_emit_primop_int64_negate() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Negate,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -42);
    }
}

#[test]
fn test_emit_primop_int64_lt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Lt,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_int64_le() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Le,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_int64_gt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Gt,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

#[test]
fn test_emit_primop_int64_ge() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Ge,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0);
    }
}

#[test]
fn test_emit_primop_bytearray_set() {
    let ba = VarId(1);
    let dummy = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::Var(ba),                  // 2
            CoreFrame::Lit(Literal::LitInt(2)),  // 3 (off)
            CoreFrame::Lit(Literal::LitInt(4)),  // 4 (len)
            CoreFrame::Lit(Literal::LitInt(0xFF)), // 5 (val)
            CoreFrame::PrimOp {
                op: PrimOpKind::SetByteArray,
                args: vec![2, 3, 4, 5],
            }, // 6
            CoreFrame::Var(ba),                  // 7
            CoreFrame::Lit(Literal::LitInt(2)),  // 8
            CoreFrame::PrimOp {
                op: PrimOpKind::ReadWord8Array,
                args: vec![7, 8],
            }, // 9
            CoreFrame::Var(dummy),               // 10
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![9, 10],
            }, // 11
            CoreFrame::LetNonRec {
                binder: dummy,
                rhs: 6,
                body: 11,
            }, // 12
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 1,
                body: 12,
            }, // 13
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0xFF);
    }
}

#[test]
fn test_emit_primop_bytearray_shrink() {
    let ba = VarId(1);
    let dummy = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::Var(ba),                  // 2
            CoreFrame::Lit(Literal::LitInt(5)),  // 3 (new size)
            CoreFrame::PrimOp {
                op: PrimOpKind::ShrinkMutableByteArray,
                args: vec![2, 3],
            }, // 4
            CoreFrame::Var(ba),                  // 5
            CoreFrame::PrimOp {
                op: PrimOpKind::SizeofByteArray,
                args: vec![5],
            }, // 6
            CoreFrame::Var(dummy),               // 7
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![6, 7],
            }, // 8
            CoreFrame::LetNonRec {
                binder: dummy,
                rhs: 4,
                body: 8,
            }, // 9
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 1,
                body: 9,
            }, // 10
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 5);
    }
}

#[test]
fn test_emit_primop_bytearray_sizeof_mutable() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(24)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::SizeofMutableByteArray,
                args: vec![1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 24);
    }
}

#[test]
fn test_emit_primop_bytearray_compare() {
    let ba1 = VarId(1);
    let ba2 = VarId(2);
    let d1 = VarId(3);
    let d2 = VarId(4);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 2
            CoreFrame::Var(ba1),
            CoreFrame::Lit(Literal::LitInt(0)),
            CoreFrame::Lit(Literal::LitInt(5)),
            CoreFrame::Lit(Literal::LitInt(0xAA)),
            CoreFrame::PrimOp {
                op: PrimOpKind::SetByteArray,
                args: vec![3, 4, 5, 6],
            }, // 7
            CoreFrame::Var(ba2),
            CoreFrame::PrimOp {
                op: PrimOpKind::SetByteArray,
                args: vec![8, 4, 5, 6],
            }, // 9
            CoreFrame::Var(ba1),
            CoreFrame::Var(ba2),
            CoreFrame::PrimOp {
                op: PrimOpKind::CompareByteArrays,
                args: vec![10, 4, 11, 4, 5],
            }, // 12
            CoreFrame::Var(d1),
            CoreFrame::Var(d2),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![12, 13],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![15, 14],
            },
            CoreFrame::LetNonRec {
                binder: d2,
                rhs: 9,
                body: 16,
            },
            CoreFrame::LetNonRec {
                binder: d1,
                rhs: 7,
                body: 17,
            },
            CoreFrame::LetNonRec {
                binder: ba2,
                rhs: 2,
                body: 18,
            },
            CoreFrame::LetNonRec {
                binder: ba1,
                rhs: 1,
                body: 19,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0);
    }
}

#[test]
fn test_emit_primop_int64_shl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Shl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1024);
    }
}

#[test]
fn test_emit_primop_int64_shra() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(-16)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64Shra,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -4);
    }
}

#[test]
fn test_emit_primop_word64_and() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0xFF00)),
            CoreFrame::Lit(Literal::LitWord(0x0FF0)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word64And,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr) as u64, 0x0F00);
    }
}

#[test]
fn test_emit_primop_word64_or() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0xF000)),
            CoreFrame::Lit(Literal::LitWord(0x000F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word64Or,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr) as u64, 0xF00F);
    }
}

#[test]
fn test_emit_primop_word64_shl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(1)),
            CoreFrame::Lit(Literal::LitInt(32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word64Shl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr) as u64, 1u64 << 32);
    }
}

#[test]
fn test_emit_int64_to_int_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64ToInt,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_error_sentinel_let_non_rec_deferred() {
    // let x = error_var in 42
    // x should be deferred (poison closure) and not crash.
    let error_var = VarId(0x4500000000000002); // UserError
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(error_var),           // 0: RHS (error_var)
            CoreFrame::Lit(Literal::LitInt(42)), // 1: body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2: root
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
    assert!(host_fns::take_runtime_error().is_none());
}

#[test]
fn test_error_sentinel_let_non_rec_forced() {
    // let x = error_var in x
    // x is forced, should return poison closure and set error flag.
    let error_var = VarId(0x4500000000000002);
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(error_var), // 0: RHS
            CoreFrame::Var(x),         // 1: body
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2: root
        ],
    };
    let result = compile_and_run(&tree);
    // Should be the lazy poison closure for kind 2 (UserError)
    assert_eq!(result.result_ptr, host_fns::error_poison_ptr_lazy(2));
    let err = host_fns::take_runtime_error();
    assert!(err.is_none()); // Error flag not set — lazy poison defers until call.
}

#[test]
fn test_error_sentinel_let_rec_deferred() {
    // let rec x = error_var; y = \n -> n in y 42
    // x is a direct error call (Var with 0x45 tag), so it gets poisoned.
    // y 42 runs fine and returns 42 with no runtime error.
    let error_var = VarId(0x4500000000000002);
    let x = VarId(1);
    let y = VarId(2);
    let n = VarId(3);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(error_var),             // 0: x rhs (direct error)
            CoreFrame::Var(n),                     // 1: y lambda body
            CoreFrame::Lam { binder: n, body: 1 }, // 2: y rhs
            CoreFrame::Lit(Literal::LitInt(42)),   // 3: 42
            CoreFrame::Var(y),                     // 4: y
            CoreFrame::App { fun: 4, arg: 3 },     // 5: y 42
            CoreFrame::LetRec {
                bindings: vec![(x, 0), (y, 2)],
                body: 5,
            }, // 6: root
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
    assert!(host_fns::take_runtime_error().is_none());
}

#[test]
fn test_error_sentinel_let_rec_no_rec_deferred() {
    // let rec x = error_var in 42
    // This hits the simple-only path in LetRec which correctly has the fix.
    let error_var = VarId(0x4500000000000002);
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(error_var),           // 0
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::LetRec {
                bindings: vec![(x, 0)],
                body: 1,
            }, // 2: root
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
    assert!(host_fns::take_runtime_error().is_none());
}

#[test]
fn test_error_sentinel_detection_in_complex_rhs() {
    // let x = App(error_var, Lit(0)) in x
    // x RHS is an App chain headed by error_var → detected as direct error call.
    // Body returns x, which should be the poison closure.
    let error_var = VarId(0x4500000000000002);
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(error_var),          // 0: error head
            CoreFrame::Lit(Literal::LitInt(0)), // 1: arg
            CoreFrame::App { fun: 0, arg: 1 },  // 2: App(error_var, 0)
            CoreFrame::Var(x),                  // 3: body (return x)
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 2,
                body: 3,
            }, // 4: root
        ],
    };
    let result = compile_and_run(&tree);
    assert_eq!(result.result_ptr, host_fns::error_poison_ptr_lazy(2));
    assert!(host_fns::take_runtime_error().is_none());
}

#[test]
fn test_non_error_sentinel_not_deferred() {
    // let x = 42 in x
    // x is not an error sentinel, should be evaluated normally.
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(x),                   // 1
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            }, // 2
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_int_to_int64_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntToInt64,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_int64_to_word64_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(-1)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int64ToWord64,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr) as u64, u64::MAX);
    }
}

#[test]
fn test_emit_word64_to_int64_conversion() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(u64::MAX)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word64ToInt64,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), -1);
    }
}

#[test]
fn test_emit_primop_bytearray_compare_unequal() {
    let ba1 = VarId(1);
    let ba2 = VarId(2);
    let d1 = VarId(3);
    let d2 = VarId(4);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 2
            CoreFrame::Var(ba1),
            CoreFrame::Lit(Literal::LitInt(0)),
            CoreFrame::Lit(Literal::LitInt(5)),
            CoreFrame::Lit(Literal::LitInt(0xAA)),
            CoreFrame::PrimOp {
                op: PrimOpKind::SetByteArray,
                args: vec![3, 4, 5, 6],
            }, // 7
            CoreFrame::Var(ba2),
            CoreFrame::Lit(Literal::LitInt(0xBB)),
            CoreFrame::PrimOp {
                op: PrimOpKind::SetByteArray,
                args: vec![8, 4, 5, 9],
            }, // 10
            CoreFrame::Var(ba1),
            CoreFrame::Var(ba2),
            CoreFrame::PrimOp {
                op: PrimOpKind::CompareByteArrays,
                args: vec![11, 4, 12, 4, 5],
            }, // 13
            CoreFrame::Var(d1),
            CoreFrame::Var(d2),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![13, 14],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![16, 15],
            },
            CoreFrame::LetNonRec {
                binder: d2,
                rhs: 10,
                body: 17,
            },
            CoreFrame::LetNonRec {
                binder: d1,
                rhs: 7,
                body: 18,
            },
            CoreFrame::LetNonRec {
                binder: ba2,
                rhs: 2,
                body: 19,
            },
            CoreFrame::LetNonRec {
                binder: ba1,
                rhs: 1,
                body: 20,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!(read_lit_int(result.result_ptr) != 0);
    }
}

#[test]
fn test_emit_primop_bytearray_copy_mutable() {
    let ba1 = VarId(1);
    let ba2 = VarId(2);
    let d1 = VarId(3);
    let d2 = VarId(4);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 2
            CoreFrame::Var(ba1),
            CoreFrame::Lit(Literal::LitInt(0)),
            CoreFrame::Lit(Literal::LitInt(5)),
            CoreFrame::Lit(Literal::LitInt(0x42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::SetByteArray,
                args: vec![3, 4, 5, 6],
            }, // 7
            CoreFrame::Var(ba1),
            CoreFrame::Var(ba2),
            CoreFrame::PrimOp {
                op: PrimOpKind::CopyMutableByteArray,
                args: vec![8, 4, 9, 4, 5],
            }, // 10
            CoreFrame::Var(ba2),
            CoreFrame::PrimOp {
                op: PrimOpKind::ReadWord8Array,
                args: vec![11, 4],
            }, // 12
            CoreFrame::Var(d1),
            CoreFrame::Var(d2),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![12, 13],
            },
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![15, 14],
            },
            CoreFrame::LetNonRec {
                binder: d2,
                rhs: 10,
                body: 16,
            },
            CoreFrame::LetNonRec {
                binder: d1,
                rhs: 7,
                body: 17,
            },
            CoreFrame::LetNonRec {
                binder: ba2,
                rhs: 2,
                body: 18,
            },
            CoreFrame::LetNonRec {
                binder: ba1,
                rhs: 1,
                body: 19,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 0x42);
    }
}

#[test]
fn test_emit_primop_bytearray_index_word8() {
    let ba = VarId(1);
    let d1 = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::Var(ba),
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::Lit(Literal::LitWord(123)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WriteWord8Array,
                args: vec![2, 3, 4],
            }, // 5
            CoreFrame::Var(ba),
            CoreFrame::PrimOp {
                op: PrimOpKind::UnsafeFreezeByteArray,
                args: vec![6],
            }, // 7
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IndexWord8Array,
                args: vec![7, 8],
            }, // 9
            CoreFrame::Var(d1),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![9, 10],
            }, // 11
            CoreFrame::LetNonRec {
                binder: d1,
                rhs: 5,
                body: 11,
            },
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 1,
                body: 12,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 123);
    }
}

#[test]
fn test_emit_primop_bytearray_copy_addr() {
    let ba = VarId(1);
    let d1 = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitString(b"ABCDE".to_vec())), // 0
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![1],
            }, // 2
            CoreFrame::Var(ba),
            CoreFrame::Lit(Literal::LitInt(2)), // dest off
            CoreFrame::Lit(Literal::LitInt(3)), // len
            CoreFrame::PrimOp {
                op: PrimOpKind::CopyAddrToByteArray,
                args: vec![0, 3, 4, 5],
            }, // 6
            CoreFrame::Var(ba),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::PrimOp {
                op: PrimOpKind::ReadWord8Array,
                args: vec![7, 8],
            }, // 9
            CoreFrame::Var(d1),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![9, 10],
            }, // 11
            CoreFrame::LetNonRec {
                binder: d1,
                rhs: 6,
                body: 11,
            },
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 2,
                body: 12,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 'A' as i64);
    }
}

#[test]
fn test_emit_primop_bytearray_resize() {
    let ba = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp {
                op: PrimOpKind::NewByteArray,
                args: vec![0],
            }, // 1
            CoreFrame::Var(ba),
            CoreFrame::Lit(Literal::LitInt(20)),
            CoreFrame::PrimOp {
                op: PrimOpKind::ResizeMutableByteArray,
                args: vec![2, 3],
            }, // 4
            CoreFrame::PrimOp {
                op: PrimOpKind::SizeofByteArray,
                args: vec![4],
            }, // 5
            CoreFrame::LetNonRec {
                binder: ba,
                rhs: 1,
                body: 5,
            }, // 6
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 20);
    }
}

#[test]
fn test_emit_primop_float_comparisons() {
    fn run_cmp(op: PrimOpKind, a: f32, b: f32) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitFloat(f32::to_bits(a) as u64)),
                CoreFrame::Lit(Literal::LitFloat(f32::to_bits(b) as u64)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }

    assert_eq!(run_cmp(PrimOpKind::FloatEq, 1.0, 1.0), 1);
    assert_eq!(run_cmp(PrimOpKind::FloatNe, 1.0, 2.0), 1);
    assert_eq!(run_cmp(PrimOpKind::FloatLe, 1.0, 1.0), 1);
    assert_eq!(run_cmp(PrimOpKind::FloatGt, 2.0, 1.0), 1);
    assert_eq!(run_cmp(PrimOpKind::FloatGe, 2.0, 2.0), 1);
}

#[test]
fn test_emit_primop_float_arith_extra() {
    let tree_sub = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(5.0) as u64)),
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(3.0) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatSub,
                args: vec![0, 1],
            },
        ],
    };
    let res_sub = compile_and_run(&tree_sub);
    unsafe {
        assert_eq!(read_lit_float(res_sub.result_ptr), 2.0);
    }

    let tree_div = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(10.0) as u64)),
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(4.0) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatDiv,
                args: vec![0, 1],
            },
        ],
    };
    let res_div = compile_and_run(&tree_div);
    unsafe {
        assert_eq!(read_lit_float(res_div.result_ptr), 2.5);
    }
}

#[test]
fn test_emit_primop_double_comparisons() {
    fn run_cmp(op: PrimOpKind, a: f64, b: f64) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitDouble(f64::to_bits(a))),
                CoreFrame::Lit(Literal::LitDouble(f64::to_bits(b))),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }

    assert_eq!(run_cmp(PrimOpKind::DoubleNe, 1.0, 2.0), 1);
    assert_eq!(run_cmp(PrimOpKind::DoubleLe, 1.0, 1.0), 1);
    assert_eq!(run_cmp(PrimOpKind::DoubleGt, 2.0, 1.0), 1);
    assert_eq!(run_cmp(PrimOpKind::DoubleGe, 2.0, 2.0), 1);
}

#[test]
fn test_emit_primop_char_comparisons() {
    fn run_cmp(op: PrimOpKind, a: char, b: char) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitChar(a)),
                CoreFrame::Lit(Literal::LitChar(b)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }

    assert_eq!(run_cmp(PrimOpKind::CharNe, 'a', 'b'), 1);
    assert_eq!(run_cmp(PrimOpKind::CharLe, 'a', 'a'), 1);
    assert_eq!(run_cmp(PrimOpKind::CharGt, 'b', 'a'), 1);
    assert_eq!(run_cmp(PrimOpKind::CharGe, 'b', 'a'), 1);
}

#[test]
fn test_emit_primop_word_comparisons_extra() {
    fn run_cmp(op: PrimOpKind, a: u64, b: u64) -> i64 {
        let tree = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitWord(a)),
                CoreFrame::Lit(Literal::LitWord(b)),
                CoreFrame::PrimOp {
                    op,
                    args: vec![0, 1],
                },
            ],
        };
        let result = compile_and_run(&tree);
        unsafe { read_lit_int(result.result_ptr) }
    }

    assert_eq!(run_cmp(PrimOpKind::WordEq, 42, 42), 1);
    assert_eq!(run_cmp(PrimOpKind::WordNe, 42, 43), 1);
}

#[test]
fn test_emit_primop_word_bitwise_extra() {
    let tree_xor = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0xFF)),
            CoreFrame::Lit(Literal::LitWord(0x0F)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordXor,
                args: vec![0, 1],
            },
        ],
    };
    let res_xor = compile_and_run(&tree_xor);
    unsafe {
        assert_eq!(read_lit_int(res_xor.result_ptr) as u64, 0xF0);
    }

    let tree_not = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(0)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordNot,
                args: vec![0],
            },
        ],
    };
    let res_not = compile_and_run(&tree_not);
    unsafe {
        assert_eq!(read_lit_int(res_not.result_ptr) as u64, u64::MAX);
    }
}

#[test]
fn test_emit_primop_conversions_extra() {
    let tree_i2w = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Int2Word,
                args: vec![0],
            },
        ],
    };
    let res_i2w = compile_and_run(&tree_i2w);
    unsafe {
        assert_eq!(read_lit_int(res_i2w.result_ptr), 42);
    }

    let tree_w2i = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitWord(42)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word2Int,
                args: vec![0],
            },
        ],
    };
    let res_w2i = compile_and_run(&tree_w2i);
    unsafe {
        assert_eq!(read_lit_int(res_w2i.result_ptr), 42);
    }

    let tree_d2f = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(1.5))),
            CoreFrame::PrimOp {
                op: PrimOpKind::Double2Float,
                args: vec![0],
            },
        ],
    };
    let res_d2f = compile_and_run(&tree_d2f);
    unsafe {
        assert_eq!(read_lit_float(res_d2f.result_ptr), 1.5);
    }

    let tree_f2i = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitFloat(f32::to_bits(3.7) as u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Float2Int,
                args: vec![0],
            },
        ],
    };
    let res_f2i = compile_and_run(&tree_f2i);
    unsafe {
        assert_eq!(read_lit_int(res_f2i.result_ptr), 3);
    }
}

// ---- Thunk tests ----

#[test]
fn test_thunk_con_basic() {
    // Con(tag=42, [Lit(1), App(identity, Lit(2))])
    // Field 0 = Lit(1) → trivial, evaluated eagerly
    // Field 1 = App(identity, Lit(2)) → non-trivial, should be thunked
    let x = VarId(0x100);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(x),                     // 0: body of identity lambda
            CoreFrame::Lit(Literal::LitInt(2)),    // 1: argument to identity
            CoreFrame::Lam { binder: x, body: 0 }, // 2: identity lambda: \x -> x
            CoreFrame::App { fun: 2, arg: 1 },     // 3: App(identity, 2)
            CoreFrame::Lit(Literal::LitInt(1)),    // 4: first field
            CoreFrame::Con {
                tag: DataConId(42),
                fields: vec![4, 3],
            }, // 5 (root)
        ],
    };

    host_fns::reset_call_depth();
    let mut result = compile_and_run(&tree);
    unsafe {
        // Result is a Con
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        assert_eq!(read_con_tag(result.result_ptr), 42);

        // Field 0: should be Lit(1) — evaluated eagerly
        let f0 = read_con_field(result.result_ptr, 0);
        assert_eq!(
            layout::read_tag(f0),
            layout::TAG_LIT,
            "field 0 should be Lit"
        );
        assert_eq!(read_lit_int(f0), 1);

        // Field 1: should be a THUNK (not yet evaluated)
        let f1 = read_con_field(result.result_ptr, 1);
        let f1_tag = layout::read_tag(f1);
        eprintln!(
            "field 1 tag = {} (expected TAG_THUNK={})",
            f1_tag,
            layout::TAG_THUNK
        );
        assert_eq!(f1_tag, layout::TAG_THUNK, "field 1 should be a thunk");

        // Force the thunk
        let f1_forced = result.force(f1);
        assert_eq!(
            layout::read_tag(f1_forced),
            layout::TAG_LIT,
            "forced thunk should be Lit"
        );
        assert_eq!(read_lit_int(f1_forced), 2, "forced thunk should be 2");
    }
}

#[test]
fn test_thunk_con_recursive() {
    // LetRec go = \n -> Con(:, [n, App(go, PrimOp(IntAdd, [n, Lit(1)]))])
    // in Case (App(go, Lit(42))) of { DataAlt(:, [x, _]) -> x }
    //
    // This tests the fundamental infinite list pattern:
    // go 42 should produce Con(:, [42, THUNK(go 43)])
    // The Case should extract the head (42) without forcing the tail thunk.

    let go = VarId(0x200);
    let n = VarId(0x300);
    let x = VarId(0x400);
    let xs = VarId(0x500);
    let cons_tag = DataConId(0xC0C0);

    let tree = RecursiveTree {
        nodes: vec![
            // Lambda body for go:
            //   Con(:, [Var(n), App(Var(go), PrimOp(IntAdd, [Var(n), Lit(1)]))])
            CoreFrame::Var(n),                  // 0: field 0 of Con (head = n)
            CoreFrame::Var(n),                  // 1: arg to IntAdd
            CoreFrame::Lit(Literal::LitInt(1)), // 2: Lit(1)
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            }, // 3: IntAdd(n, 1)
            CoreFrame::Var(go),                 // 4: go
            CoreFrame::App { fun: 4, arg: 3 },  // 5: App(go, n+1) - recursive call
            CoreFrame::Con {
                tag: cons_tag,
                fields: vec![0, 5],
            }, // 6: Con(:, [n, App(go, n+1)])
            CoreFrame::Lam { binder: n, body: 6 }, // 7: \n -> Con(:, [n, go(n+1)])
            // Case scrutinee: App(go, 42)
            CoreFrame::Var(go),                  // 8: go
            CoreFrame::Lit(Literal::LitInt(42)), // 9: Lit(42)
            CoreFrame::App { fun: 8, arg: 9 },   // 10: App(go, 42)
            // Case: match on the list, extract head
            CoreFrame::Var(x), // 11: body of case alt (return x)
            CoreFrame::Case {
                scrutinee: 10,
                binder: VarId(0x600),
                alts: vec![Alt {
                    con: AltCon::DataAlt(cons_tag),
                    binders: vec![x, xs],
                    body: 11,
                }],
            }, // 12: Case (go 42) of { (:) x _ -> x }
            // LetRec: go = \n -> ...
            CoreFrame::LetRec {
                bindings: vec![(go, 7)],
                body: 12,
            }, // 13 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        // Result should be Lit(42) — the head of go 42
        eprintln!("result tag = {}", layout::read_tag(result.result_ptr));
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit (head of go 42)"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "head of go 42 should be 42"
        );
    }
}

// =============================================================================
// BUG REPRO: ThunkCon fields not forced in strict contexts
//
// When a Con has non-trivial fields (PrimOp, App, Case), they become thunks
// via ThunkCon. These thunks must be forced when used in:
//   1. Literal case dispatch (emit_lit_dispatch)
//   2. PrimOp arguments (unbox_int / unbox_double)
// Currently neither forces thunks — they read from LIT_VALUE_OFFSET (offset 16)
// which in a thunk object is the code pointer, not a value.
// =============================================================================

#[test]
fn test_thunkcon_field_in_lit_case() {
    // Build:
    //   let boxed = Con(I#, [PrimOp(IntSub, [Lit(3), Lit(1)])])  -- ThunkCon
    //   case boxed of { I# n# ->
    //     case n# of { 2# -> Lit(42); _ -> Lit(99) }
    //   }
    //
    // Expected: 42 (thunk forces to 2, matches the 2# literal alt)
    // Bug: thunk not forced, lit dispatch reads garbage → takes default → 99

    let i_hash = DataConId(42);
    let n = VarId(0x100);
    let scrut_binder = VarId(0x200);
    let inner_binder = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            // PrimOp: 3 - 1
            CoreFrame::Lit(Literal::LitInt(3)), // 0
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: IntSub(3, 1) → non-trivial, will be thunked
            // Con(I#, [thunked_sub]) → ThunkCon
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(3 - 1)
            // Inner case: case n# of { 2# -> 42; _ -> 99 }
            CoreFrame::Var(n),                   // 4: scrutinee = n#
            CoreFrame::Lit(Literal::LitInt(42)), // 5: match body
            CoreFrame::Lit(Literal::LitInt(99)), // 6: default body
            CoreFrame::Case {
                scrutinee: 4,
                binder: inner_binder,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(2)),
                        binders: vec![],
                        body: 5,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 6,
                    },
                ],
            }, // 7: case n# of { 2# -> 42; _ -> 99 }
            // Outer case: case boxed of { I# n# -> <inner case> }
            CoreFrame::Case {
                scrutinee: 3,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 7,
                }],
            }, // 8 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunked field (3-1=2) should match literal 2# → return 42, not 99"
        );
    }
}

#[test]
fn test_thunkcon_field_in_primop() {
    // Build:
    //   let boxed = Con(I#, [PrimOp(IntSub, [Lit(10), Lit(3)])])  -- ThunkCon
    //   case boxed of { I# n# ->
    //     PrimOp(IntAdd, [n#, Lit(5)])
    //   }
    //
    // Expected: 12 (thunk forces to 7, then 7+5=12)
    // Bug: unbox_int reads garbage from thunk code_ptr → garbage result

    let i_hash = DataConId(42);
    let n = VarId(0x100);
    let scrut_binder = VarId(0x200);

    let tree = RecursiveTree {
        nodes: vec![
            // PrimOp: 10 - 3
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(3)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: IntSub(10, 3) → thunked
            // Con(I#, [thunked_sub]) → ThunkCon
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(10 - 3)
            // Body: n# + 5
            CoreFrame::Var(n),                  // 4: n# (thunked field)
            CoreFrame::Lit(Literal::LitInt(5)), // 5
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![4, 5],
            }, // 6: IntAdd(n#, 5)
            // case boxed of { I# n# -> n# + 5 }
            CoreFrame::Case {
                scrutinee: 3,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 6,
                }],
            }, // 7 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            12,
            "thunked field (10-3=7) + 5 should equal 12"
        );
    }
}

#[test]
fn test_thunkcon_recursive_countdown() {
    // The actual bug pattern from test_compose_hylo_is_fused_ana_cata:
    //   go f seed = case seed of { 0# -> 0; _ -> go f (f seed) }
    //   f = \n -> n - 1
    //   go f 3
    //
    // Expected: 0 (decrements 3→2→1→0)
    // Bug: f returns I#(n-1) as ThunkCon, go's literal case on the unboxed
    //      field reads garbage, never matches 0# → infinite recursion

    let go = VarId(0x7300_0000_0000_0014);
    let f = VarId(0x6100_0000_0000_0001);
    let seed = VarId(0x6100_0000_0000_0002);
    let n = VarId(0x6100_0000_0000_0003);
    let case_b1 = VarId(0x6100_0000_0000_0004);
    let case_b2 = VarId(0x6100_0000_0000_0005);
    let i_hash = DataConId(42);

    let tree = RecursiveTree {
        nodes: vec![
            // === go's lambda body ===
            // case seed of { I# n# -> case n# of { 0# -> I# 0; _ -> go f (f seed) } }

            // Inner literals
            CoreFrame::Lit(Literal::LitInt(0)), // 0: lit 0 for base case
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![0],
            }, // 1: I# 0 (base case result)
            // Default branch: go f (f seed)
            CoreFrame::Var(f),                 // 2
            CoreFrame::Var(seed),              // 3
            CoreFrame::App { fun: 2, arg: 3 }, // 4: f seed
            CoreFrame::Var(go),                // 5
            CoreFrame::Var(f),                 // 6
            CoreFrame::App { fun: 5, arg: 6 }, // 7: go f
            CoreFrame::App { fun: 7, arg: 4 }, // 8: go f (f seed) — TAIL CALL
            // Inner case: case n# of { 0# -> I# 0; _ -> go f (f seed) }
            CoreFrame::Var(n), // 9: scrutinee = n#
            CoreFrame::Case {
                scrutinee: 9,
                binder: case_b2,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(0)),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 8,
                    },
                ],
            }, // 10: inner case
            // Outer case: case seed of { I# n# -> <inner case> }
            CoreFrame::Var(seed), // 11: scrutinee
            CoreFrame::Case {
                scrutinee: 11,
                binder: case_b1,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 10,
                }],
            }, // 12: outer case
            // go = \f -> \seed -> <outer case>
            CoreFrame::Lam {
                binder: seed,
                body: 12,
            }, // 13
            CoreFrame::Lam {
                binder: f,
                body: 13,
            }, // 14: go = \f seed -> ...
            // === f's lambda body ===
            // \x -> case x of { I# m -> I#(m - 1) }
            CoreFrame::Var(VarId(0x6100_0000_0000_0010)), // 15: m (extracted)
            CoreFrame::Lit(Literal::LitInt(1)),           // 16
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![15, 16],
            }, // 17: m - 1 → NON-TRIVIAL → will be thunked in Con
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![17],
            }, // 18: I#(m - 1) → ThunkCon!
            CoreFrame::Var(VarId(0x6100_0000_0000_0010)), // 19: scrutinee
            CoreFrame::Case {
                scrutinee: 19,
                binder: VarId(0x6100_0000_0000_0011),
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![VarId(0x6100_0000_0000_0010)],
                    body: 18,
                }],
            }, // 20: case x of { I# m -> I#(m-1) }
            CoreFrame::Lam {
                binder: VarId(0x6100_0000_0000_0010),
                body: 20,
            }, // 21: f = \x -> case x of { I# m -> I#(m-1) }
            // === entry point ===
            // let boxed_3 = I# 3
            CoreFrame::Lit(Literal::LitInt(3)), // 22
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![22],
            }, // 23: I# 3
            // go f (I# 3)
            CoreFrame::Var(go),                  // 24
            CoreFrame::Var(f),                   // 25
            CoreFrame::App { fun: 24, arg: 25 }, // 26: go f
            CoreFrame::App { fun: 26, arg: 23 }, // 27: go f (I# 3)
            // LetRec
            CoreFrame::LetRec {
                bindings: vec![(go, 14), (f, 21)],
                body: 27,
            }, // 28 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_CON,
            "result should be Con (I# 0)"
        );
        let inner = read_con_field(result.result_ptr, 0);
        assert_eq!(layout::read_tag(inner), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(inner),
            0,
            "go (\\n -> n-1) (I# 3) should count down to I# 0"
        );
    }
}

// =============================================================================
// Laziness boundary crossing tests
//
// These test compositions of lazy producers (ThunkCon fields) with strict
// consumers (literal case, data case, PrimOp, function application) that must
// force thunks before reading heap layout.
// =============================================================================

#[test]
fn test_thunkcon_primop_field_in_data_case() {
    // P1×C2: PrimOp thunk field → data case dispatch
    // Con(Just, [IntSub(3,1)]) → case x of { Just v -> v }
    // Expected: thunked field forces to Lit(2)

    let just_tag = DataConId(1);
    let v = VarId(0x100);
    let scrut_binder = VarId(0x200);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(3)), // 0
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: IntSub(3, 1) → non-trivial, thunked in Con
            CoreFrame::Con {
                tag: just_tag,
                fields: vec![2],
            }, // 3: Just(thunked 3-1)
            CoreFrame::Var(v),                  // 4: extracted field
            CoreFrame::Case {
                scrutinee: 3,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(just_tag),
                    binders: vec![v],
                    body: 4,
                }],
            }, // 5 (root)
        ],
    };

    host_fns::reset_call_depth();
    let mut result = compile_and_run(&tree);
    unsafe {
        // Field v is a thunk (lazy case alt extraction), force it explicitly
        let forced = result.force(result.result_ptr);
        assert_eq!(layout::read_tag(forced), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(forced),
            2,
            "thunked IntSub(3,1) should force to 2"
        );
    }
}

#[test]
fn test_thunkcon_app_field_in_lit_case() {
    // P2×C1: App thunk field → literal case dispatch
    // identity = \x -> x
    // Con(I#, [App(identity, Lit(5))]) → case boxed of { I# n -> case n of { 5# -> 42; _ -> 99 } }
    // Expected: 42 (thunk forces to 5, matches 5# alt)

    let i_hash = DataConId(42);
    let x = VarId(0x100);
    let n = VarId(0x200);
    let scrut_binder = VarId(0x300);
    let inner_binder = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            // identity = \x -> x
            CoreFrame::Var(x),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1: identity
            // App(identity, Lit(5)) → non-trivial, thunked
            CoreFrame::Lit(Literal::LitInt(5)), // 2
            CoreFrame::App { fun: 1, arg: 2 },  // 3: identity 5
            // Con(I#, [thunked_app])
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![3],
            }, // 4: I#(identity 5) → ThunkCon
            // Inner case: case n# of { 5# -> 42; _ -> 99 }
            CoreFrame::Var(n),                   // 5: scrutinee = n#
            CoreFrame::Lit(Literal::LitInt(42)), // 6: match body
            CoreFrame::Lit(Literal::LitInt(99)), // 7: default body
            CoreFrame::Case {
                scrutinee: 5,
                binder: inner_binder,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(5)),
                        binders: vec![],
                        body: 6,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 7,
                    },
                ],
            }, // 8: literal dispatch forces thunked n#
            // Outer case: case boxed of { I# n# -> <inner> }
            CoreFrame::Case {
                scrutinee: 4,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 8,
                }],
            }, // 9 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunked App(identity, 5) should force to 5, matching 5# → 42"
        );
    }
}

#[test]
fn test_thunkcon_app_field_in_primop() {
    // P2×C3: App thunk field → PrimOp argument
    // identity = \x -> x
    // Con(I#, [App(identity, Lit(7))]) → case boxed of { I# n -> n + 3 }
    // Expected: 10 (thunk forces to 7, then 7+3=10)

    let i_hash = DataConId(42);
    let x = VarId(0x100);
    let n = VarId(0x200);
    let scrut_binder = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            // identity = \x -> x
            CoreFrame::Var(x),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1: identity
            // App(identity, Lit(7)) → non-trivial, thunked
            CoreFrame::Lit(Literal::LitInt(7)), // 2
            CoreFrame::App { fun: 1, arg: 2 },  // 3: identity 7
            // Con(I#, [thunked_app])
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![3],
            }, // 4: I#(identity 7) → ThunkCon
            // Body: n# + 3
            CoreFrame::Var(n),                  // 5: n# (thunked field)
            CoreFrame::Lit(Literal::LitInt(3)), // 6
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![5, 6],
            }, // 7: IntAdd forces thunked n#
            // case boxed of { I# n# -> n# + 3 }
            CoreFrame::Case {
                scrutinee: 4,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 7,
                }],
            }, // 8 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            10,
            "thunked App(identity, 7) + 3 should equal 10"
        );
    }
}

#[test]
fn test_thunkcon_case_field_in_lit_case() {
    // P3×C1: Case thunk field → literal case dispatch
    // Con(I#, [Case(Lit(1), {1# -> Lit(5); _ -> Lit(0)})]) → unbox → lit case on 5
    // Expected: 42 (case evaluates to 5, lit dispatch matches 5# → 42)

    let i_hash = DataConId(42);
    let n = VarId(0x100);
    let scrut_binder = VarId(0x200);
    let inner_case_binder = VarId(0x300);
    let outer_lit_binder = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            // Case expression: case 1 of { 1# -> 5; _ -> 0 } → non-trivial, thunked
            CoreFrame::Lit(Literal::LitInt(1)), // 0: scrutinee
            CoreFrame::Lit(Literal::LitInt(5)), // 1: match body
            CoreFrame::Lit(Literal::LitInt(0)), // 2: default body
            CoreFrame::Case {
                scrutinee: 0,
                binder: inner_case_binder,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(1)),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 2,
                    },
                ],
            }, // 3: evaluates to 5
            // Con(I#, [thunked_case])
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![3],
            }, // 4: I#(case result) → ThunkCon
            // Literal case: case n# of { 5# -> 42; _ -> 99 }
            CoreFrame::Var(n),                   // 5: n# (thunked field)
            CoreFrame::Lit(Literal::LitInt(42)), // 6: match body
            CoreFrame::Lit(Literal::LitInt(99)), // 7: default body
            CoreFrame::Case {
                scrutinee: 5,
                binder: outer_lit_binder,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(5)),
                        binders: vec![],
                        body: 6,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 7,
                    },
                ],
            }, // 8: literal dispatch forces thunked n#
            // Outer case: case boxed of { I# n# -> <lit case> }
            CoreFrame::Case {
                scrutinee: 4,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 8,
                }],
            }, // 9 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunked Case(1, 1#->5) should force to 5, matching 5# → 42"
        );
    }
}

#[test]
fn test_thunkcon_nested_in_primop() {
    // P4×C3: Nested Con thunk → PrimOp
    // Con(I#, [Con(I#, [IntSub(10,3)])]) → case of I# boxed2 → case of I# n → n + 1
    // Expected: 8 (inner IntSub=7, double indirection through two thunk layers, 7+1=8)
    //
    // This exercises both data case dispatch forcing (C2, for boxed2) and
    // PrimOp arg forcing (C3, for n).

    let i_hash = DataConId(42);
    let n = VarId(0x100);
    let boxed2 = VarId(0x200);
    let outer_binder = VarId(0x300);
    let inner_binder = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            // IntSub(10, 3) → non-trivial
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(3)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: IntSub(10,3)
            // Inner Con(I#, [thunked_sub]) → ThunkCon (PrimOp field)
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(10-3)
            // Outer Con(I#, [inner_con]) → ThunkCon (inner Con is non-trivial)
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![3],
            }, // 4: I#(I#(10-3))
            // Body: n + 1
            CoreFrame::Var(n),                  // 5: n (from inner unbox)
            CoreFrame::Lit(Literal::LitInt(1)), // 6
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![5, 6],
            }, // 7: n + 1 (PrimOp forces thunked n)
            // Inner case: case boxed2 of { I# n -> n + 1 }
            CoreFrame::Var(boxed2), // 8: boxed2 (thunk, forced by data dispatch)
            CoreFrame::Case {
                scrutinee: 8,
                binder: inner_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 7,
                }],
            }, // 9: data dispatch forces boxed2 thunk
            // Outer case: case outer of { I# boxed2 -> <inner case> }
            CoreFrame::Case {
                scrutinee: 4,
                binder: outer_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![boxed2],
                    body: 9,
                }],
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_LIT,
            "result should be Lit"
        );
        assert_eq!(
            read_lit_int(result.result_ptr),
            8,
            "nested thunks: IntSub(10,3)=7, unwrap twice, 7+1=8"
        );
    }
}

// =============================================================================
// Adversarial JIT tests: compositions that stress boundary assumptions
// =============================================================================

#[test]
fn test_adversarial_multi_field_thunkcon() {
    // Multi-field Con where ALL fields are non-trivial → both thunked.
    // Tests that field offsets are correct when multiple thunks are stored.
    //
    // Con(Pair, [IntSub(10,3), IntMul(4,5)])
    // case pair of { Pair a b -> a + b }
    // Expected: 7 + 20 = 27

    let pair_tag = DataConId(0);
    let a = VarId(0x100);
    let b = VarId(0x200);
    let scrut_binder = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            // Field 0: IntSub(10, 3) = 7
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(3)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: non-trivial → thunked
            // Field 1: IntMul(4, 5) = 20
            CoreFrame::Lit(Literal::LitInt(4)), // 3
            CoreFrame::Lit(Literal::LitInt(5)), // 4
            CoreFrame::PrimOp {
                op: PrimOpKind::IntMul,
                args: vec![3, 4],
            }, // 5: non-trivial → thunked
            // Con(Pair, [thunk0, thunk1])
            CoreFrame::Con {
                tag: pair_tag,
                fields: vec![2, 5],
            }, // 6: ThunkCon with 2 fields
            // Body: a + b
            CoreFrame::Var(a), // 7
            CoreFrame::Var(b), // 8
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![7, 8],
            }, // 9: forces both thunks
            // case pair of { Pair a b -> a + b }
            CoreFrame::Case {
                scrutinee: 6,
                binder: scrut_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(pair_tag),
                    binders: vec![a, b],
                    body: 9,
                }],
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            27,
            "Pair(IntSub(10,3), IntMul(4,5)): field0=7 + field1=20 = 27"
        );
    }
}

#[test]
fn test_adversarial_thunk_forces_to_con_in_data_dispatch() {
    // A thunked field evaluates to a Con (not a Lit). Data case dispatch on
    // the thunked field must: detect TAG_THUNK (tag < 2) → heap_force → get Con →
    // dispatch on con_tag.
    //
    // Con(Box=0, [Case(1, { 1# -> Just(42), _ -> Nothing })])
    // case box of { Box inner ->
    //   case inner of { Just v -> v; Nothing -> 0 }
    // }
    // Expected: 42

    let box_tag = DataConId(0);
    let just_tag = DataConId(1);
    let nothing_tag = DataConId(2);
    let inner = VarId(0x100);
    let v = VarId(0x200);
    let b1 = VarId(0x300);
    let b2 = VarId(0x400);
    let b3 = VarId(0x500);

    let tree = RecursiveTree {
        nodes: vec![
            // Inner case: case 1 of { 1# -> Just(42); _ -> Nothing }
            CoreFrame::Lit(Literal::LitInt(1)),  // 0: scrutinee
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::Con {
                tag: just_tag,
                fields: vec![1],
            }, // 2: Just(42) — trivial field → regular Con
            CoreFrame::Con {
                tag: nothing_tag,
                fields: vec![],
            }, // 3: Nothing
            CoreFrame::Case {
                scrutinee: 0,
                binder: b1,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(1)),
                        binders: vec![],
                        body: 2,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 3,
                    },
                ],
            }, // 4: Case → non-trivial → thunked in outer Con
            // Outer Con(Box, [thunked_case])
            CoreFrame::Con {
                tag: box_tag,
                fields: vec![4],
            }, // 5: Box(thunk) → ThunkCon
            // Inner dispatch: case inner of { Just v -> v; Nothing -> 0 }
            CoreFrame::Var(inner), // 6: inner (thunk from Box extraction)
            CoreFrame::Var(v),     // 7: v from Just
            CoreFrame::Lit(Literal::LitInt(0)), // 8: Nothing branch
            CoreFrame::Case {
                scrutinee: 6,
                binder: b2,
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(just_tag),
                        binders: vec![v],
                        body: 7,
                    },
                    Alt {
                        con: AltCon::DataAlt(nothing_tag),
                        binders: vec![],
                        body: 8,
                    },
                ],
            }, // 9: data dispatch on inner — must force thunk!
            // Outer dispatch: case box of { Box inner -> <inner dispatch> }
            CoreFrame::Case {
                scrutinee: 5,
                binder: b3,
                alts: vec![Alt {
                    con: AltCon::DataAlt(box_tag),
                    binders: vec![inner],
                    body: 9,
                }],
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunk forces to Just(42), data dispatch matches Just, returns 42"
        );
    }
}

#[test]
fn test_adversarial_thunked_closure_in_app_fun() {
    // A thunked field evaluates to a Closure. App must force the fun position
    // (tag check → heap_force) before calling.
    //
    // identity = \x -> x
    // closure = \y -> y + 10
    // box = Con(Box=0, [App(identity, closure)])   — App is non-trivial → thunked
    // case box of { Box f -> f 5 }
    // Expected: 15

    let box_tag = DataConId(0);
    let x = VarId(0x100);
    let y = VarId(0x200);
    let f = VarId(0x300);
    let scrut_b = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            // identity = \x -> x
            CoreFrame::Var(x),                     // 0
            CoreFrame::Lam { binder: x, body: 0 }, // 1
            // closure = \y -> y + 10
            CoreFrame::Var(y),                   // 2
            CoreFrame::Lit(Literal::LitInt(10)), // 3
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![2, 3],
            }, // 4: y + 10
            CoreFrame::Lam { binder: y, body: 4 }, // 5: \y -> y + 10
            // App(identity, closure) → non-trivial → thunked in Con
            CoreFrame::App { fun: 1, arg: 5 }, // 6: identity(closure) = closure
            // Con(Box, [thunked App])
            CoreFrame::Con {
                tag: box_tag,
                fields: vec![6],
            }, // 7: Box(thunk) → ThunkCon
            // Body: f 5
            CoreFrame::Var(f),                  // 8: f (thunked closure)
            CoreFrame::Lit(Literal::LitInt(5)), // 9
            CoreFrame::App { fun: 8, arg: 9 },  // 10: f 5 — App forces f (TAG_THUNK → heap_force)
            // case box of { Box f -> f 5 }
            CoreFrame::Case {
                scrutinee: 7,
                binder: scrut_b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(box_tag),
                    binders: vec![f],
                    body: 10,
                }],
            }, // 11 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            15,
            "thunked App(identity, \\y->y+10) forces to closure, applied to 5 = 15"
        );
    }
}

#[test]
fn test_adversarial_join_point_thunked_arg() {
    // Jump passes a thunked value to a join point. The join body uses it in
    // a PrimOp which must force it. Tests Jump → block param → PrimOp forcing.
    //
    // join j n = n + 1
    // in
    //   case Con(I#, [IntSub(10,3)]) of { I# field -> jump j field }
    // Expected: 8 (field=thunk(7), join body forces → 7+1=8)

    let i_hash = DataConId(42);
    let j = JoinId(1);
    let n = VarId(0x100);
    let field = VarId(0x200);
    let scrut_b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            // Join RHS: n + 1
            CoreFrame::Var(n),                  // 0
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: n + 1 (PrimOp forces thunked n)
            // Join body:
            CoreFrame::Lit(Literal::LitInt(10)), // 3
            CoreFrame::Lit(Literal::LitInt(3)),  // 4
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![3, 4],
            }, // 5: IntSub(10,3) → thunked in Con
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![5],
            }, // 6: I#(thunked 7)
            CoreFrame::Var(field),               // 7: extracted field (thunk)
            CoreFrame::Jump {
                label: j,
                args: vec![7],
            }, // 8: jump j field
            CoreFrame::Case {
                scrutinee: 6,
                binder: scrut_b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![field],
                    body: 8,
                }],
            }, // 9: case ... of { I# field -> jump j field }
            // Join
            CoreFrame::Join {
                label: j,
                params: vec![n],
                rhs: 2,
                body: 9,
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            8,
            "jump passes thunked field(7) to join, PrimOp forces → 7+1=8"
        );
    }
}

#[test]
fn test_adversarial_litchar_thunk_dispatch() {
    // LitChar case dispatch on a thunked value. Tests literal dispatch
    // with char comparison (icmp on codepoints).
    //
    // Con(I#, [IntAdd(65, 1)]) → case of { I# n -> case n of { 'B' -> 42; _ -> 99 } }
    // Expected: 42 (65+1=66='B', matches)

    let i_hash = DataConId(42);
    let n = VarId(0x100);
    let scrut_b = VarId(0x200);
    let inner_b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(65)), // 0: ord 'A'
            CoreFrame::Lit(Literal::LitInt(1)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: 66 = ord 'B', thunked
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(thunked 66)
            // Inner: case n of { 'B' -> 42; _ -> 99 }
            CoreFrame::Var(n),                   // 4
            CoreFrame::Lit(Literal::LitInt(42)), // 5
            CoreFrame::Lit(Literal::LitInt(99)), // 6
            CoreFrame::Case {
                scrutinee: 4,
                binder: inner_b,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitChar('B')),
                        binders: vec![],
                        body: 5,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 6,
                    },
                ],
            }, // 7
            // Outer: case boxed of { I# n -> <inner> }
            CoreFrame::Case {
                scrutinee: 3,
                binder: scrut_b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 7,
                }],
            }, // 8 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunked IntAdd(65,1)=66='B', char dispatch matches 'B' → 42"
        );
    }
}

#[test]
fn test_adversarial_double_thunk_in_lit_dispatch() {
    // LitDouble case dispatch on a thunked double value. Tests the F64 bitcast
    // path in emit_lit_dispatch with thunk forcing.
    //
    // Con(D#, [DoubleSub(3.5, 1.0)]) → case of { D# d -> case d of { 2.5 -> 42; _ -> 99 } }
    // Expected: 42 (3.5-1.0=2.5, matches)

    let d_hash = DataConId(99);
    let d = VarId(0x100);
    let scrut_b = VarId(0x200);
    let inner_b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(3.5))), // 0
            CoreFrame::Lit(Literal::LitDouble(f64::to_bits(1.0))), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleSub,
                args: vec![0, 1],
            }, // 2: 2.5, thunked
            CoreFrame::Con {
                tag: d_hash,
                fields: vec![2],
            }, // 3: D#(thunked 2.5)
            // Inner: case d of { 2.5 -> 42; _ -> 99 }
            CoreFrame::Var(d),                   // 4
            CoreFrame::Lit(Literal::LitInt(42)), // 5
            CoreFrame::Lit(Literal::LitInt(99)), // 6
            CoreFrame::Case {
                scrutinee: 4,
                binder: inner_b,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitDouble(f64::to_bits(2.5))),
                        binders: vec![],
                        body: 5,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 6,
                    },
                ],
            }, // 7
            // Outer: case boxed of { D# d -> <inner> }
            CoreFrame::Case {
                scrutinee: 3,
                binder: scrut_b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(d_hash),
                    binders: vec![d],
                    body: 7,
                }],
            }, // 8 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunked DoubleSub(3.5,1.0)=2.5, double lit dispatch matches 2.5 → 42"
        );
    }
}

#[test]
fn test_adversarial_thunked_arg_through_app() {
    // App does NOT force args — only the fun position. The arg passes through
    // as a thunk to the lambda body, which must force it in a strict context.
    //
    // Con(I#, [IntSub(10, 3)]) →
    //   case boxed of { I# field ->
    //     (\x -> x + 1) field     -- field is thunk, passed as arg, forced in body
    //   }
    // Expected: 8

    let i_hash = DataConId(42);
    let x = VarId(0x100);
    let field = VarId(0x200);
    let scrut_b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(3)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: thunked
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(thunked 7)
            // Lambda: \x -> x + 1
            CoreFrame::Var(x),                  // 4
            CoreFrame::Lit(Literal::LitInt(1)), // 5
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![4, 5],
            }, // 6: x + 1
            CoreFrame::Lam { binder: x, body: 6 }, // 7: \x -> x + 1
            // App(\x -> x+1, field)
            CoreFrame::Var(field),             // 8: field (thunk)
            CoreFrame::App { fun: 7, arg: 8 }, // 9: apply — arg NOT forced by App
            // case boxed of { I# field -> <App> }
            CoreFrame::Case {
                scrutinee: 3,
                binder: scrut_b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![field],
                    body: 9,
                }],
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            8,
            "thunked arg passed through App, lambda body forces via PrimOp → 7+1=8"
        );
    }
}

#[test]
fn test_adversarial_multi_alt_after_thunk_force() {
    // Data case dispatch forces a thunk that evaluates to one of several
    // constructors. Tests that the forced result's con_tag is read correctly
    // and the right alt is selected.
    //
    // x = Con(Box, [Case(1, { 0# -> Left(99), _ -> Right(42) })])
    // case box of { Box inner ->
    //   case inner of { Left n -> n; Right n -> n }
    // }
    // Expected: 42 (1 matches default → Right(42))

    let box_tag = DataConId(0);
    let left_tag = DataConId(1);
    let right_tag = DataConId(2);
    let inner = VarId(0x100);
    let n = VarId(0x200);
    let b1 = VarId(0x300);
    let b2 = VarId(0x400);
    let b3 = VarId(0x500);

    let tree = RecursiveTree {
        nodes: vec![
            // case 1 of { 0# -> Left(99); _ -> Right(42) }
            CoreFrame::Lit(Literal::LitInt(1)),  // 0: scrutinee
            CoreFrame::Lit(Literal::LitInt(99)), // 1
            CoreFrame::Con {
                tag: left_tag,
                fields: vec![1],
            }, // 2: Left(99)
            CoreFrame::Lit(Literal::LitInt(42)), // 3
            CoreFrame::Con {
                tag: right_tag,
                fields: vec![3],
            }, // 4: Right(42)
            CoreFrame::Case {
                scrutinee: 0,
                binder: b1,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(0)),
                        binders: vec![],
                        body: 2,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 4,
                    },
                ],
            }, // 5: evaluates to Right(42) → non-trivial → thunked
            // Box(thunked_case)
            CoreFrame::Con {
                tag: box_tag,
                fields: vec![5],
            }, // 6: ThunkCon
            // case inner of { Left n -> n; Right n -> n }
            CoreFrame::Var(inner), // 7
            CoreFrame::Var(n),     // 8: n from either branch
            CoreFrame::Case {
                scrutinee: 7,
                binder: b2,
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(left_tag),
                        binders: vec![n],
                        body: 8,
                    },
                    Alt {
                        con: AltCon::DataAlt(right_tag),
                        binders: vec![n],
                        body: 8,
                    },
                ],
            }, // 9: data dispatch on thunked inner
            // case box of { Box inner -> <dispatch> }
            CoreFrame::Case {
                scrutinee: 6,
                binder: b3,
                alts: vec![Alt {
                    con: AltCon::DataAlt(box_tag),
                    binders: vec![inner],
                    body: 9,
                }],
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "thunk forces to Right(42), data dispatch selects Right alt → 42"
        );
    }
}

// ==========================================================================
// Holistic coverage pass: LetNonRec, LetRec, Join, default-only, zero-field
// ==========================================================================

#[test]
fn test_holistic_let_nonrec_thunked_con_rhs() {
    // LetNonRec where rhs is a Con with non-trivial field → thunk created
    // Body uses the binding in a PrimOp (forces it).
    //
    // let x = Con(I#, [IntSub(10,3)]) in
    //   case x of { I# n -> n + 1 }
    // Expected: 8 (IntSub→7, then 7+1=8)

    let i_hash = DataConId(42);
    let x = VarId(0x100);
    let n = VarId(0x200);
    let b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(3)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![0, 1],
            }, // 2: 10-3 → non-trivial → thunked in Con
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(thunk(7))
            // Body: case x of { I# n -> n + 1 }
            CoreFrame::Var(x),                  // 4
            CoreFrame::Var(n),                  // 5
            CoreFrame::Lit(Literal::LitInt(1)), // 6
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![5, 6],
            }, // 7: n + 1
            CoreFrame::Case {
                scrutinee: 4,
                binder: b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 7,
                }],
            }, // 8
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 3,
                body: 8,
            }, // 9 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            8,
            "LetNonRec binds thunked Con, case+PrimOp forces to 8"
        );
    }
}

#[test]
fn test_holistic_letrec_case_scrutinee() {
    // LetRec binding used as case scrutinee.
    // Tests that LetRec-allocated closure is correctly evaluated
    // when used in a case expression.
    //
    // let rec f = \x. x + 1
    // in case f 5 of { 6# -> 100; _ -> 0 }
    // Expected: 100 (f 5 = 6, matches 6#)

    let f = VarId(0x100);
    let x = VarId(0x200);
    let b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            // f body: x + 1
            CoreFrame::Var(x),                  // 0
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: x + 1
            CoreFrame::Lam { binder: x, body: 2 }, // 3: \x -> x + 1
            // Body: case f 5 of { 6# -> 100; _ -> 0 }
            CoreFrame::Var(f),                    // 4
            CoreFrame::Lit(Literal::LitInt(5)),   // 5
            CoreFrame::App { fun: 4, arg: 5 },    // 6: f 5
            CoreFrame::Lit(Literal::LitInt(100)), // 7
            CoreFrame::Lit(Literal::LitInt(0)),   // 8
            CoreFrame::Case {
                scrutinee: 6,
                binder: b,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(6)),
                        binders: vec![],
                        body: 7,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 8,
                    },
                ],
            }, // 9: case f 5 of ...
            CoreFrame::LetRec {
                bindings: vec![(f, 3)],
                body: 9,
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            100,
            "LetRec closure applied in case scrutinee, lit dispatch matches 6#"
        );
    }
}

#[test]
fn test_holistic_default_only_case_thunked_scrutinee() {
    // Default-only case does NOT force the scrutinee in emit_case.
    // If the body uses the case binder in a strict context (PrimOp),
    // the PrimOp forcing must handle it.
    //
    // let x = Con(I#, [IntAdd(3,4)]) in
    //   case x of { _ -> case x of { I# n -> n + 10 } }
    // Expected: 17 (thunk(7), data case forces → 7, then 7+10=17)

    let i_hash = DataConId(42);
    let x = VarId(0x100);
    let n = VarId(0x200);
    let b1 = VarId(0x300);
    let b2 = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(3)), // 0
            CoreFrame::Lit(Literal::LitInt(4)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: 3+4 → non-trivial → thunked
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(thunk(7))
            // Inner case: case x of { I# n -> n + 10 }
            CoreFrame::Var(x),                   // 4
            CoreFrame::Var(n),                   // 5
            CoreFrame::Lit(Literal::LitInt(10)), // 6
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![5, 6],
            }, // 7: n + 10
            CoreFrame::Case {
                scrutinee: 4,
                binder: b2,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 7,
                }],
            }, // 8: data dispatch forces the Con
            // Outer case: case x of { _ -> inner_case }
            CoreFrame::Var(x), // 9
            CoreFrame::Case {
                scrutinee: 9,
                binder: b1,
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 8,
                }],
            }, // 10: default-only case (does NOT force)
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 3,
                body: 10,
            }, // 11 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            17,
            "default-only case passes thunked Con through, inner data case forces correctly"
        );
    }
}

#[test]
fn test_holistic_zero_field_con() {
    // Zero-field constructor: Con(Unit, [])
    // Tests that Con allocation handles empty fields without off-by-one.
    //
    // case Con(True, []) of { True -> 1; False -> 0 }
    // Expected: 1

    let true_tag = DataConId(1);
    let false_tag = DataConId(0);
    let b = VarId(0x100);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Con {
                tag: true_tag,
                fields: vec![],
            }, // 0: True
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::Lit(Literal::LitInt(0)), // 2
            CoreFrame::Case {
                scrutinee: 0,
                binder: b,
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(true_tag),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::DataAlt(false_tag),
                        binders: vec![],
                        body: 2,
                    },
                ],
            }, // 3 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            1,
            "zero-field Con(True) matches DataAlt(True)"
        );
    }
}

#[test]
fn test_holistic_nested_let_nonrec_thunk_chain() {
    // Chain of LetNonRec bindings where each depends on the previous,
    // and values flow through thunked Con fields.
    //
    // let a = Con(I#, [IntMul(3,4)])      -- thunk(12)
    // in let b = Con(I#, [IntAdd(5,6)])   -- thunk(11)
    // in case a of { I# x ->
    //      case b of { I# y ->
    //        x + y
    //      }
    //    }
    // Expected: 23 (12 + 11)

    let i_hash = DataConId(42);
    let a = VarId(0x100);
    let b = VarId(0x200);
    let x = VarId(0x300);
    let y = VarId(0x400);
    let b1 = VarId(0x500);
    let b2 = VarId(0x600);

    let tree = RecursiveTree {
        nodes: vec![
            // a rhs
            CoreFrame::Lit(Literal::LitInt(3)), // 0
            CoreFrame::Lit(Literal::LitInt(4)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntMul,
                args: vec![0, 1],
            }, // 2: thunked
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(thunk(12))
            // b rhs
            CoreFrame::Lit(Literal::LitInt(5)), // 4
            CoreFrame::Lit(Literal::LitInt(6)), // 5
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![4, 5],
            }, // 6: thunked
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![6],
            }, // 7: I#(thunk(11))
            // body: case a of { I# x -> case b of { I# y -> x + y } }
            CoreFrame::Var(a), // 8
            CoreFrame::Var(b), // 9
            CoreFrame::Var(x), // 10
            CoreFrame::Var(y), // 11
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![10, 11],
            }, // 12: x + y
            CoreFrame::Case {
                scrutinee: 9,
                binder: b2,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![y],
                    body: 12,
                }],
            }, // 13: inner case
            CoreFrame::Case {
                scrutinee: 8,
                binder: b1,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![x],
                    body: 13,
                }],
            }, // 14: outer case
            CoreFrame::LetNonRec {
                binder: b,
                rhs: 7,
                body: 14,
            }, // 15
            CoreFrame::LetNonRec {
                binder: a,
                rhs: 3,
                body: 15,
            }, // 16 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            23,
            "chained LetNonRec thunked Cons: 12 + 11 = 23"
        );
    }
}

#[test]
fn test_holistic_letrec_con_with_closure_sibling() {
    // LetRec with a Con binding that references a sibling closure binding.
    // Tests the deferred Con field filling in LetRec phases.
    //
    // let rec f = \x. x + 1
    //         pair = Con(Pair, [Lit(10), f])
    // in case pair of { Pair a g -> g a }
    // Expected: 11 (g=f, a=10, f 10 = 11)

    let pair_tag = DataConId(77);
    let f = VarId(0x100);
    let pair = VarId(0x200);
    let x = VarId(0x300);
    let a = VarId(0x400);
    let g = VarId(0x500);
    let b = VarId(0x600);

    let tree = RecursiveTree {
        nodes: vec![
            // f body: x + 1
            CoreFrame::Var(x),                  // 0
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: x + 1
            CoreFrame::Lam { binder: x, body: 2 }, // 3: \x -> x + 1
            // pair: Con(Pair, [Lit(10), Var(f)])
            CoreFrame::Lit(Literal::LitInt(10)), // 4
            CoreFrame::Var(f),                   // 5
            CoreFrame::Con {
                tag: pair_tag,
                fields: vec![4, 5],
            }, // 6: Pair(10, f) — trivial fields (Lit, Var)
            // body: case pair of { Pair a g -> g a }
            CoreFrame::Var(pair),              // 7
            CoreFrame::Var(g),                 // 8
            CoreFrame::Var(a),                 // 9
            CoreFrame::App { fun: 8, arg: 9 }, // 10: g a
            CoreFrame::Case {
                scrutinee: 7,
                binder: b,
                alts: vec![Alt {
                    con: AltCon::DataAlt(pair_tag),
                    binders: vec![a, g],
                    body: 10,
                }],
            }, // 11
            CoreFrame::LetRec {
                bindings: vec![(f, 3), (pair, 6)],
                body: 11,
            }, // 12 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            11,
            "LetRec Con with closure sibling: f 10 = 11"
        );
    }
}

#[test]
fn test_holistic_join_in_letrec_body() {
    // Join point used inside a LetRec body.
    // Tests interaction between join blocks and LetRec-allocated closures.
    //
    // let rec f = \x. x * 2
    // in join j n = f n
    //    in case Lit(3) of { 3# -> jump j 5; _ -> jump j 0 }
    // Expected: 10 (3 matches 3# → jump j 5 → f 5 → 5*2=10)

    let f = VarId(0x100);
    let x = VarId(0x200);
    let n = VarId(0x300);
    let j = JoinId(1);
    let b = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            // f body: x * 2
            CoreFrame::Var(x),                  // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntMul,
                args: vec![0, 1],
            }, // 2: x * 2
            CoreFrame::Lam { binder: x, body: 2 }, // 3: \x -> x * 2
            // Join rhs: f n
            CoreFrame::Var(f),                 // 4
            CoreFrame::Var(n),                 // 5
            CoreFrame::App { fun: 4, arg: 5 }, // 6: f n
            // Join body: case 3 of { 3# -> jump j 5; _ -> jump j 0 }
            CoreFrame::Lit(Literal::LitInt(3)), // 7: scrutinee
            CoreFrame::Lit(Literal::LitInt(5)), // 8: arg for jump
            CoreFrame::Jump {
                label: j,
                args: vec![8],
            }, // 9: jump j 5
            CoreFrame::Lit(Literal::LitInt(0)), // 10
            CoreFrame::Jump {
                label: j,
                args: vec![10],
            }, // 11: jump j 0
            CoreFrame::Case {
                scrutinee: 7,
                binder: b,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(3)),
                        binders: vec![],
                        body: 9,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 11,
                    },
                ],
            }, // 12
            CoreFrame::Join {
                label: j,
                params: vec![n],
                rhs: 6,
                body: 12,
            }, // 13
            CoreFrame::LetRec {
                bindings: vec![(f, 3)],
                body: 13,
            }, // 14 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            10,
            "Join in LetRec body: case 3# -> jump j 5 -> f 5 = 10"
        );
    }
}

#[test]
fn test_holistic_multi_layer_thunk_force() {
    // A thunked Con field that, when forced, produces another Con with
    // a thunked field. Tests that the data dispatch tag < 2 check + heap_force
    // handles multi-layer resolution.
    //
    // inner = Con(I#, [IntAdd(1,2)])    -- thunked field: thunk(3)
    // outer = Con(Box, [inner])          -- inner is non-trivial (Con with thunk) → thunked
    //
    // Actually, Con(I#, [IntAdd(1,2)]) has a non-trivial field so
    // is_trivial_field returns false → it becomes a thunk when nested.
    //
    // case outer of { Box payload ->
    //   case payload of { I# n -> n + 100 }
    // }
    // Expected: 103 (1+2=3, 3+100=103)

    let i_hash = DataConId(42);
    let box_tag = DataConId(0);
    let payload = VarId(0x100);
    let n = VarId(0x200);
    let b1 = VarId(0x300);
    let b2 = VarId(0x400);

    let tree = RecursiveTree {
        nodes: vec![
            // inner Con
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: non-trivial → thunked in inner Con
            CoreFrame::Con {
                tag: i_hash,
                fields: vec![2],
            }, // 3: I#(thunk(3)) — has non-trivial field
            // This Con itself has a non-trivial field (PrimOp), so when
            // it's a field of outer, it's also non-trivial → double thunking

            // outer Con
            CoreFrame::Con {
                tag: box_tag,
                fields: vec![3],
            }, // 4: Box(thunk(I#(thunk(3))))
            // body
            CoreFrame::Var(payload),              // 5
            CoreFrame::Var(n),                    // 6
            CoreFrame::Lit(Literal::LitInt(100)), // 7
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![6, 7],
            }, // 8: n + 100
            CoreFrame::Case {
                scrutinee: 5,
                binder: b2,
                alts: vec![Alt {
                    con: AltCon::DataAlt(i_hash),
                    binders: vec![n],
                    body: 8,
                }],
            }, // 9: inner data dispatch (forces payload thunk → I#)
            CoreFrame::Case {
                scrutinee: 4,
                binder: b1,
                alts: vec![Alt {
                    con: AltCon::DataAlt(box_tag),
                    binders: vec![payload],
                    body: 9,
                }],
            }, // 10 (root): outer data dispatch
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            103,
            "multi-layer thunk: outer forces to I#(thunk(3)), inner forces to 3, 3+100=103"
        );
    }
}

#[test]
fn test_holistic_case_binder_reuse() {
    // The case binder (bound to scrutinee) is used directly in the alt body
    // alongside extracted fields. Tests that case binder binding is correct.
    //
    // case Con(Pair, [Lit(10), Lit(20)]) of pair { Pair a b ->
    //   case pair of { Pair x y -> x + y + a }
    // }
    // Expected: 40 (x=10, y=20, a=10, x+y+a = 40)
    //
    // Actually GHC Core uses case binder as the whole scrutinee. So we can
    // do a data case on it again.

    let pair_tag = DataConId(77);
    let a_var = VarId(0x100);
    let b_var = VarId(0x200);
    let x_var = VarId(0x300);
    let y_var = VarId(0x400);
    let pair_binder = VarId(0x500);
    let b2 = VarId(0x600);

    let tree = RecursiveTree {
        nodes: vec![
            // scrutinee: Pair(10, 20)
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(20)), // 1
            CoreFrame::Con {
                tag: pair_tag,
                fields: vec![0, 1],
            }, // 2: Pair(10, 20) — trivial fields
            // Inner case body: x + y + a
            CoreFrame::Var(x_var), // 3
            CoreFrame::Var(y_var), // 4
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![3, 4],
            }, // 5: x + y
            CoreFrame::Var(a_var), // 6
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![5, 6],
            }, // 7: (x+y) + a
            // Inner case: case pair of { Pair x y -> x+y+a }
            CoreFrame::Var(pair_binder), // 8
            CoreFrame::Case {
                scrutinee: 8,
                binder: b2,
                alts: vec![Alt {
                    con: AltCon::DataAlt(pair_tag),
                    binders: vec![x_var, y_var],
                    body: 7,
                }],
            }, // 9
            // Outer case: case Pair(10,20) of pair { Pair a b -> inner }
            CoreFrame::Case {
                scrutinee: 2,
                binder: pair_binder,
                alts: vec![Alt {
                    con: AltCon::DataAlt(pair_tag),
                    binders: vec![a_var, b_var],
                    body: 9,
                }],
            }, // 10 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            40,
            "case binder reuse: outer binds pair, inner re-cases on it → x+y+a = 10+20+10"
        );
    }
}

#[test]
fn test_holistic_letrec_recursive_with_case() {
    // Recursive LetRec function with Case in body.
    // Tests the common GHC Core pattern of recursive function with
    // literal case dispatch on the recursive parameter.
    //
    // let rec f = \n. case n of { 0# -> 42; _ -> f (n - 1) }
    // in f 5
    // Expected: 42 (counts down from 5 to 0)

    let f = VarId(0x100);
    let n = VarId(0x200);
    let b = VarId(0x300);

    let tree = RecursiveTree {
        nodes: vec![
            // f body: case n of { 0# -> 42; _ -> f(n-1) }
            CoreFrame::Var(n),                   // 0: n (scrutinee)
            CoreFrame::Lit(Literal::LitInt(42)), // 1: base case result
            // default: f(n-1)
            CoreFrame::Var(n),                  // 2
            CoreFrame::Lit(Literal::LitInt(1)), // 3
            CoreFrame::PrimOp {
                op: PrimOpKind::IntSub,
                args: vec![2, 3],
            }, // 4: n - 1
            CoreFrame::Var(f),                  // 5
            CoreFrame::App { fun: 5, arg: 4 },  // 6: f (n-1)
            CoreFrame::Case {
                scrutinee: 0,
                binder: b,
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(0)),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 6,
                    },
                ],
            }, // 7: case n of ...
            CoreFrame::Lam { binder: n, body: 7 }, // 8: \n -> case n of ...
            // main body: f 5
            CoreFrame::Var(f),                  // 9
            CoreFrame::Lit(Literal::LitInt(5)), // 10
            CoreFrame::App { fun: 9, arg: 10 }, // 11: f 5
            CoreFrame::LetRec {
                bindings: vec![(f, 8)],
                body: 11,
            }, // 12 (root)
        ],
    };

    host_fns::reset_call_depth();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(
            read_lit_int(result.result_ptr),
            42,
            "recursive LetRec with case: f counts down 5→0, returns 42"
        );
    }
}
