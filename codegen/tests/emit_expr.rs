use codegen::context::VMContext;
use codegen::pipeline::CodegenPipeline;
use codegen::host_fns;
use codegen::emit::expr::compile_expr;
use core_repr::*;
use core_heap::layout;

struct TestResult {
    result_ptr: *const u8,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

/// Helper: set up pipeline + nursery, compile expr, call it, return result ptr.
fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize();
    
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
    let tree = RecursiveTree { nodes: vec![CoreFrame::Lit(Literal::LitInt(42))] };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_emit_primop_int_add() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(1)),    // 0
        CoreFrame::Lit(Literal::LitInt(2)),    // 1
        CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2 (root)
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 3); }
}

#[test]
fn test_emit_con_fields() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(10)),   // 0
        CoreFrame::Lit(Literal::LitInt(20)),   // 1
        CoreFrame::Con { tag: DataConId(5), fields: vec![0, 1] }, // 2 (root)
    ] };
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
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),           // 0: arg
        CoreFrame::Var(x),                              // 1: body (x)
        CoreFrame::Lam { binder: x, body: 1 },         // 2: λx.x
        CoreFrame::App { fun: 2, arg: 0 },             // 3: (λx.x) 42 (root)
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_emit_closure_capture() {
    let x = VarId(1);
    let y = VarId(2);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(1)),                             // 0: y = 1
        CoreFrame::Var(x),                                               // 1: x
        CoreFrame::Var(y),                                               // 2: y
        CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] }, // 3: x + y
        CoreFrame::Lam { binder: x, body: 3 },                          // 4: λx. x + y
        CoreFrame::Lit(Literal::LitInt(2)),                             // 5: arg = 2
        CoreFrame::App { fun: 4, arg: 5 },                              // 6: (λx. x+y) 2
        CoreFrame::LetNonRec { binder: y, rhs: 0, body: 6 },           // 7: let y=1 in ... (root)
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 3); }
}

#[test]
fn test_emit_let_non_rec() {
    let x = VarId(1);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),                            // 0
        CoreFrame::Var(x),                                               // 1
        CoreFrame::LetNonRec { binder: x, rhs: 0, body: 1 },           // 2 (root)
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_emit_primop_int_sub() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(10)),
        CoreFrame::Lit(Literal::LitInt(3)),
        CoreFrame::PrimOp { op: PrimOpKind::IntSub, args: vec![0, 1] },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 7); }
}

#[test]
fn test_emit_primop_int_mul() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(6)),
        CoreFrame::Lit(Literal::LitInt(7)),
        CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![0, 1] },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_emit_primop_int_negate() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),
        CoreFrame::PrimOp { op: PrimOpKind::IntNegate, args: vec![0] },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), -42); }
}

#[test]
fn test_emit_primop_int_comparisons() {
    fn run_cmp(op: PrimOpKind, a: i64, b: i64) -> i64 {
        let tree = RecursiveTree { nodes: vec![
            CoreFrame::Lit(Literal::LitInt(a)),
            CoreFrame::Lit(Literal::LitInt(b)),
            CoreFrame::PrimOp { op, args: vec![0, 1] },
        ] };
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
        let tree = RecursiveTree { nodes: vec![
            CoreFrame::Lit(Literal::LitWord(a)),
            CoreFrame::Lit(Literal::LitWord(b)),
            CoreFrame::PrimOp { op, args: vec![0, 1] },
        ] };
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
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitWord(1)),
        CoreFrame::Lit(Literal::LitWord(2)),
        CoreFrame::PrimOp { op: PrimOpKind::WordAdd, args: vec![0, 1] },
    ] };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        // Offset 8 is lit_tag
        assert_eq!(*result.result_ptr.add(8), codegen::emit::LIT_TAG_WORD as u8);
        assert_eq!(read_lit_int(result.result_ptr), 3);
    }
}

#[test]
fn test_emit_let_rec() {
    // let rec f = λn. if n == 0 then 1 else n * f (n-1) in f 5
    // Simplified: let rec f = λx. x in f 42
    let f = VarId(1);
    let x = VarId(2);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),                                      // 0: x
        CoreFrame::Lam { binder: x, body: 0 },                 // 1: λx.x
        CoreFrame::Lit(Literal::LitInt(42)),                    // 2: arg 42
        CoreFrame::Var(f),                                      // 3: f
        CoreFrame::App { fun: 3, arg: 2 },                     // 4: f 42
        CoreFrame::LetRec { bindings: vec![(f, 1)], body: 4 }, // 5: let rec f = ... (root)
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_emit_let_rec_factorial_ish() {
    // let rec f = λn. if n == 0 then 1 else n * f (n-1)
    // Since I don't have Case/If yet, I'll test a simple mutually recursive pair or just recursion.
    // let rec f = λn. n in f 10
    let f = VarId(1);
    let n = VarId(2);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(n),                                       // 0: n
        CoreFrame::Lam { binder: n, body: 0 },                  // 1: f = λn. n
        CoreFrame::Lit(Literal::LitInt(10)),                     // 2: 10
        CoreFrame::Var(f),                                       // 3: f
        CoreFrame::App { fun: 3, arg: 2 },                      // 4: f 10
        CoreFrame::LetRec { bindings: vec![(f, 1)], body: 4 },  // 5: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 10); }
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
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x_id),                                   // 0: x
        CoreFrame::Lam { binder: x_id, body: 0 },              // 1: f = λx.x
        CoreFrame::Var(f_id),                                   // 2: f
        CoreFrame::Var(y_id),                                   // 3: y
        CoreFrame::App { fun: 2, arg: 3 },                     // 4: f y
        CoreFrame::Lam { binder: y_id, body: 4 },              // 5: g = λy. f y
        CoreFrame::Lit(Literal::LitInt(42)),                    // 6: 42
        CoreFrame::Var(g_id),                                   // 7: g
        CoreFrame::App { fun: 7, arg: 6 },                     // 8: g 42
        CoreFrame::LetRec { bindings: vec![(f_id, 1), (g_id, 5)], body: 8 }, // 9: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_emit_unique_lambda_names() {
    // Compile two different expressions that both have lambdas using the same pipeline.
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    
    let x = VarId(1);
    // λx.x
    let tree1 = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),
        CoreFrame::Lam { binder: x, body: 0 },
    ] };
    
    // λx. x + 1
    let tree2 = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),
        CoreFrame::Lit(Literal::LitInt(1)),
        CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] },
        CoreFrame::Lam { binder: x, body: 2 },
    ] };
    
        // This should NOT panic due to "duplicate function name"
    
        compile_expr(&mut pipeline, &tree1, "f1").expect("First compilation failed");
    
        compile_expr(&mut pipeline, &tree2, "f2").expect("Second compilation failed");

    }

#[test]
fn test_emit_runtime_error_div_zero() {
    // A Var with tag 'E' (0x45) and kind 0 should call runtime_error(0) and return null
    let div_zero_var = VarId(0x4500000000000000); // tag 'E', kind 0 = divZero
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(div_zero_var), // 0 (root)
    ] };
    let result = compile_and_run(&tree);
    assert!(result.result_ptr.is_null());
    let err = host_fns::take_runtime_error();
    assert!(matches!(err, Some(host_fns::RuntimeError::DivisionByZero)));
}

#[test]
fn test_emit_runtime_error_overflow() {
    let overflow_var = VarId(0x4500000000000001); // tag 'E', kind 1 = overflow
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(overflow_var), // 0 (root)
    ] };
    let result = compile_and_run(&tree);
    assert!(result.result_ptr.is_null());
    let err = host_fns::take_runtime_error();
    assert!(matches!(err, Some(host_fns::RuntimeError::Overflow)));
}

#[test]
fn test_emit_int_quot() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),
        CoreFrame::Lit(Literal::LitInt(6)),
        CoreFrame::PrimOp { op: PrimOpKind::IntQuot, args: vec![0, 1] },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 7); }
}

#[test]
fn test_emit_int_rem() {
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),
        CoreFrame::Lit(Literal::LitInt(5)),
        CoreFrame::PrimOp { op: PrimOpKind::IntRem, args: vec![0, 1] },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 2); }
}
    