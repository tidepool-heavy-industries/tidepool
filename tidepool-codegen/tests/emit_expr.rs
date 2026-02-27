use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;

struct TestResult {
    result_ptr: *const u8,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

/// Helper: set up pipeline + nursery, compile expr, call it, return result ptr.
fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536]; // 64KB nursery
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    TestResult {
        result_ptr: result as *const u8,
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
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());

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
    let result = compile_and_run(&tree);
    unsafe {
        let f0 = read_con_field(result.result_ptr, 0);
        let f1 = read_con_field(result.result_ptr, 1);
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
    let cases = vec![
        (0x01, 7),
        (0x80, 0),
        (0x00, 8),
        (0xFF, 0),
        (0x40, 1),
    ];
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
    let result = compile_and_run(&tree);
    unsafe {
        let f0 = read_con_field(result.result_ptr, 0); // carry
        let f1 = read_con_field(result.result_ptr, 1); // val
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
            CoreFrame::Var(error_var),          // 0: x rhs (direct error)
            CoreFrame::Var(n),                  // 1: y lambda body
            CoreFrame::Lam { binder: n, body: 1 }, // 2: y rhs
            CoreFrame::Lit(Literal::LitInt(42)), // 3: 42
            CoreFrame::Var(y),                  // 4: y
            CoreFrame::App { fun: 4, arg: 3 },  // 5: y 42
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
