use tidepool_codegen::context::VMContext;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_codegen::host_fns;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_repr::*;
use tidepool_heap::layout;

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

/// Helper: read field i from a ConObject.
unsafe fn read_con_field(ptr: *const u8, i: usize) -> *const u8 {
    *(ptr.add(24 + 8 * i) as *const *const u8)
}

#[test]
fn test_join_basic() {
    // join j(x) = x in jump j(42)
    let j = JoinId(1);
    let x = VarId(1);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),                                       // 0: rhs (x)
        CoreFrame::Lit(Literal::LitInt(42)),                     // 1: jump arg
        CoreFrame::Jump { label: j, args: vec![1] },            // 2: jump j(42) — body
        CoreFrame::Join { label: j, params: vec![x], rhs: 0, body: 2 }, // 3: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_join_two_params() {
    // join j(a, b) = a + b in jump j(10, 20)
    let j = JoinId(1);
    let a = VarId(1);
    let b = VarId(2);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(a),                                       // 0
        CoreFrame::Var(b),                                       // 1
        CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: a + b (rhs)
        CoreFrame::Lit(Literal::LitInt(10)),                     // 3
        CoreFrame::Lit(Literal::LitInt(20)),                     // 4
        CoreFrame::Jump { label: j, args: vec![3, 4] },         // 5: body
        CoreFrame::Join { label: j, params: vec![a, b], rhs: 2, body: 5 }, // 6: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 30); }
}

#[test]
fn test_join_body_falls_through() {
    // join j(x) = x in 99
    // Body doesn't jump, so result is 99
    let j = JoinId(1);
    let x = VarId(1);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),                                       // 0: rhs
        CoreFrame::Lit(Literal::LitInt(99)),                     // 1: body (no jump)
        CoreFrame::Join { label: j, params: vec![x], rhs: 0, body: 1 }, // 2: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 99); }
}

#[test]
fn test_join_nested_inner_shadows() {
    // join j(x) = x
    // in join j(y) = y + 1   -- shadows outer j
    //    in jump j(10)        -- jumps to inner j
    // Result: 11
    let j = JoinId(1);
    let x = VarId(1);
    let y = VarId(2);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),                                       // 0: outer rhs
        CoreFrame::Var(y),                                       // 1
        CoreFrame::Lit(Literal::LitInt(1)),                      // 2
        CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] }, // 3: y + 1 (inner rhs)
        CoreFrame::Lit(Literal::LitInt(10)),                     // 4: jump arg
        CoreFrame::Jump { label: j, args: vec![4] },            // 5: jump j(10)
        CoreFrame::Join { label: j, params: vec![y], rhs: 3, body: 5 }, // 6: inner join
        CoreFrame::Join { label: j, params: vec![x], rhs: 0, body: 6 }, // 7: outer join (root)
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 11); }
}

#[test]
fn test_join_with_heap_ptrs() {
    // join j(x) = Con(0, [x]) in jump j(Lit(42))
    // Verify heap pointer handling through join block params
    let j = JoinId(1);
    let x = VarId(1);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),                                       // 0
        CoreFrame::Con { tag: DataConId(0), fields: vec![0] },  // 1: rhs = Con(0, [x])
        CoreFrame::Lit(Literal::LitInt(42)),                     // 2
        CoreFrame::Jump { label: j, args: vec![2] },            // 3: body
        CoreFrame::Join { label: j, params: vec![x], rhs: 1, body: 3 }, // 4: root
    ] };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        let field = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(field), 42);
    }
}
