// Tests use 3.14 / 3.14159 as round-trip float literals for primop codegen,
// not as math constants.
#![allow(clippy::approx_constant)]

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;

struct TestResult {
    result_ptr: *const u8,
    _vmctx: VMContext,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

impl Drop for TestResult {
    fn drop(&mut self) {
        host_fns::clear_gc_state();
        host_fns::clear_stack_map_registry();
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
        _vmctx: vmctx,
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

unsafe fn read_lit_word(ptr: *const u8) -> u64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    *(ptr.add(16) as *const u64)
}

// Float arithmetic (11 tests)

#[test]
fn test_emit_primop_float_add() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.5f32)),
            CoreFrame::Lit(Literal::from(2.5f32)),
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
fn test_emit_primop_float_sub() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(5.0f32)),
            CoreFrame::Lit(Literal::from(3.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatSub,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), 2.0);
    }
}

#[test]
fn test_emit_primop_float_mul() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(3.0f32)),
            CoreFrame::Lit(Literal::from(4.0f32)),
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
fn test_emit_primop_float_div() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(10.0f32)),
            CoreFrame::Lit(Literal::from(4.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatDiv,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), 2.5);
    }
}

#[test]
fn test_emit_primop_float_negate() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(3.5f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatNegate,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_float(result.result_ptr), -3.5);
    }
}

#[test]
fn test_emit_primop_float_eq() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatEq,
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
fn test_emit_primop_float_ne() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::Lit(Literal::from(2.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatNe,
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
fn test_emit_primop_float_lt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::Lit(Literal::from(2.0f32)),
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
fn test_emit_primop_float_le() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatLe,
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
fn test_emit_primop_float_gt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(2.0f32)),
            CoreFrame::Lit(Literal::from(1.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatGt,
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
fn test_emit_primop_float_ge() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(2.0f32)),
            CoreFrame::Lit(Literal::from(2.0f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::FloatGe,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1);
    }
}

// Float conversions (4 tests)

#[test]
fn test_emit_primop_float_2_int() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(3.7f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Float2Int,
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
fn test_emit_primop_int_2_float() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(42i64)),
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
fn test_emit_primop_float_2_double() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(3.14f32)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Float2Double,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        let val = read_lit_double(result.result_ptr);
        assert!((val - 3.14).abs() < 1e-6);
    }
}

#[test]
fn test_emit_primop_double_2_float() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(3.1415926535f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Double2Float,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        let val = read_lit_float(result.result_ptr);
        assert!((val - 3.141_592_7_f32).abs() < 1e-6);
    }
}

// Transcendental Double (13 tests)

#[test]
fn test_emit_primop_double_sqrt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(4.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleSqrt,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 2.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_exp() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleExp,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 1.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_log() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(std::f64::consts::E)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleLog,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 1.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_power() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(2.0f64)),
            CoreFrame::Lit(Literal::from(10.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoublePower,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 1024.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_sin() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleSin,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 0.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_cos() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleCos,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 1.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_tan() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleTan,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 0.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_asin() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleAsin,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_acos() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleAcos,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 0.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_atan() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleAtan,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - std::f64::consts::FRAC_PI_4).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_sinh() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleSinh,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 0.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_cosh() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleCosh,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 1.0).abs() < 1e-9);
    }
}

#[test]
fn test_emit_primop_double_tanh() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0.0f64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::DoubleTanh,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert!((read_lit_double(result.result_ptr) - 0.0).abs() < 1e-9);
    }
}

// Integer bitwise (7 tests)

#[test]
fn test_emit_primop_int_shl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1i64)),
            CoreFrame::Lit(Literal::from(4i64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntShl,
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
fn test_emit_primop_int_shra() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(-16i64)),
            CoreFrame::Lit(Literal::from(2i64)),
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
fn test_emit_primop_int_shrl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(16i64)),
            CoreFrame::Lit(Literal::from(2i64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntShrl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 4);
    }
}

#[test]
fn test_emit_primop_int_and() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0xFFi64)),
            CoreFrame::Lit(Literal::from(0x0Fi64)),
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
            CoreFrame::Lit(Literal::from(0xF0i64)),
            CoreFrame::Lit(Literal::from(0x0Fi64)),
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
            CoreFrame::Lit(Literal::from(0xFFi64)),
            CoreFrame::Lit(Literal::from(0x0Fi64)),
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
            CoreFrame::Lit(Literal::from(0i64)),
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

// Word operations (7 tests)

#[test]
fn test_emit_primop_word_shl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(1u64)),
            CoreFrame::Lit(Literal::from(8i64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordShl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 256);
    }
}

#[test]
fn test_emit_primop_word_shrl() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(256u64)),
            CoreFrame::Lit(Literal::from(4i64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordShrl,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 16);
    }
}

#[test]
fn test_emit_primop_word_and() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0xFFu64)),
            CoreFrame::Lit(Literal::from(0x0Fu64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordAnd,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 0x0F);
    }
}

#[test]
fn test_emit_primop_word_or() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0xF0u64)),
            CoreFrame::Lit(Literal::from(0x0Fu64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordOr,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 0xFF);
    }
}

#[test]
fn test_emit_primop_word_xor() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0xFFu64)),
            CoreFrame::Lit(Literal::from(0x0Fu64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordXor,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 0xF0);
    }
}

#[test]
fn test_emit_primop_word_not() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordNot,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), !0u64);
    }
}

#[test]
fn test_emit_primop_word_mul() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(123u64)),
            CoreFrame::Lit(Literal::from(456u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordMul,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 123 * 456);
    }
}

// Word8 operations (5 tests)

#[test]
fn test_emit_primop_word8_add() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(200u64)),
            CoreFrame::Lit(Literal::from(100u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word8Add,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        // 200 + 100 = 300, 300 % 256 = 44
        assert_eq!(read_lit_word(result.result_ptr), 44);
    }
}

#[test]
fn test_emit_primop_word8_sub() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(10u64)),
            CoreFrame::Lit(Literal::from(20u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word8Sub,
                args: vec![0, 1],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        // 10 - 20 = -10, which wraps to 246 (masking to 8 bits)
        assert_eq!(read_lit_word(result.result_ptr), 246);
    }
}

#[test]
fn test_emit_primop_word8_lt() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(10u64)),
            CoreFrame::Lit(Literal::from(20u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word8Lt,
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
fn test_emit_primop_word_to_word8() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0x1234u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::WordToWord8,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 0x34);
    }
}

#[test]
fn test_emit_primop_word8_to_word() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::from(0x34u64)),
            CoreFrame::PrimOp {
                op: PrimOpKind::Word8ToWord,
                args: vec![0],
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_word(result.result_ptr), 0x34);
    }
}
