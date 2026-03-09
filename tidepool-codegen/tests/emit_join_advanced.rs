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

fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
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

unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    *(ptr.add(16) as *const i64)
}

#[test]
fn test_nested_joins() {
    // join j1(x) = join j2(y) = x + y in jump j2(10) in jump j1(20)
    let j1 = JoinId(1);
    let j2 = JoinId(2);
    let x = VarId(1);
    let y = VarId(2);

    let mut bld = TreeBuilder::new();
    // Inner join j2(y) = x + y
    let vx = bld.push(CoreFrame::Var(x));
    let vy = bld.push(CoreFrame::Var(y));
    let add = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![vx, vy],
    });
    let lit10 = bld.push(CoreFrame::Lit(Literal::LitInt(10)));
    let jump_j2 = bld.push(CoreFrame::Jump {
        label: j2,
        args: vec![lit10],
    });
    let inner_join = bld.push(CoreFrame::Join {
        label: j2,
        params: vec![y],
        rhs: add,
        body: jump_j2,
    });

    // Outer join j1(x) = inner_join in jump j1(20)
    let lit20 = bld.push(CoreFrame::Lit(Literal::LitInt(20)));
    let jump_j1 = bld.push(CoreFrame::Jump {
        label: j1,
        args: vec![lit20],
    });
    bld.push(CoreFrame::Join {
        label: j1,
        params: vec![x],
        rhs: inner_join,
        body: jump_j1,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 30);
    }
}

#[test]
fn test_join_nested_jump() {
    // join j(x) = x * 2 in (jump j(jump j(5)))
    // To support nesting, we use two joins where one jumps to another
    // join j1(x) = x * 2 in
    //   join j2(y) = jump j1(y * 2) in
    //     jump j2(5)
    // Result: 20
    let j1 = JoinId(1);
    let j2 = JoinId(2);
    let x = VarId(1);
    let y = VarId(2);

    let mut bld = TreeBuilder::new();
    // j1(x) = x * 2
    let vx = bld.push(CoreFrame::Var(x));
    let lit2 = bld.push(CoreFrame::Lit(Literal::LitInt(2)));
    let mul1 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntMul,
        args: vec![vx, lit2],
    });

    // j2(y) = jump j1(y * 2)
    let vy = bld.push(CoreFrame::Var(y));
    let mul2 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntMul,
        args: vec![vy, lit2],
    });
    let jump_j1 = bld.push(CoreFrame::Jump {
        label: j1,
        args: vec![mul2],
    });

    // jump j2(5)
    let lit5 = bld.push(CoreFrame::Lit(Literal::LitInt(5)));
    let jump_j2 = bld.push(CoreFrame::Jump {
        label: j2,
        args: vec![lit5],
    });

    let inner_join = bld.push(CoreFrame::Join {
        label: j2,
        params: vec![y],
        rhs: jump_j1,
        body: jump_j2,
    });
    bld.push(CoreFrame::Join {
        label: j1,
        params: vec![x],
        rhs: mul1,
        body: inner_join,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 20);
    }
}

#[test]
fn test_join_three_params() {
    // join j(a, b, c) = a + b + c in jump j(1, 2, 3)
    let j = JoinId(1);
    let a = VarId(1);
    let b = VarId(2);
    let c = VarId(3);

    let mut bld = TreeBuilder::new();
    let va = bld.push(CoreFrame::Var(a));
    let vb = bld.push(CoreFrame::Var(b));
    let vc = bld.push(CoreFrame::Var(c));
    let add1 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![va, vb],
    });
    let add2 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![add1, vc],
    });

    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let lit2 = bld.push(CoreFrame::Lit(Literal::LitInt(2)));
    let lit3 = bld.push(CoreFrame::Lit(Literal::LitInt(3)));
    let jump = bld.push(CoreFrame::Jump {
        label: j,
        args: vec![lit1, lit2, lit3],
    });

    bld.push(CoreFrame::Join {
        label: j,
        params: vec![a, b, c],
        rhs: add2,
        body: jump,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 6);
    }
}

#[test]
fn test_join_zero_args() {
    // join j() = 42 in jump j()
    let j = JoinId(1);

    let mut bld = TreeBuilder::new();
    let lit42 = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    let jump = bld.push(CoreFrame::Jump {
        label: j,
        args: vec![],
    });
    bld.push(CoreFrame::Join {
        label: j,
        params: vec![],
        rhs: lit42,
        body: jump,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_multiple_joins() {
    // join j1(x) = x + 1 in join j2(x) = x * 2 in jump j1(jump j2(5))
    // Pattern: join j1(x) = x + 1 in join j2(y) = jump j1(y * 2) in jump j2(5)
    // Result: 11
    let j1 = JoinId(1);
    let j2 = JoinId(2);
    let x = VarId(1);
    let y = VarId(2);

    let mut bld = TreeBuilder::new();
    // j1(x) = x + 1
    let vx = bld.push(CoreFrame::Var(x));
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let add = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![vx, lit1],
    });

    // j2(y) = jump j1(y * 2)
    let vy = bld.push(CoreFrame::Var(y));
    let lit2 = bld.push(CoreFrame::Lit(Literal::LitInt(2)));
    let mul = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntMul,
        args: vec![vy, lit2],
    });
    let jump_j1 = bld.push(CoreFrame::Jump {
        label: j1,
        args: vec![mul],
    });

    // jump j2(5)
    let lit5 = bld.push(CoreFrame::Lit(Literal::LitInt(5)));
    let jump_j2 = bld.push(CoreFrame::Jump {
        label: j2,
        args: vec![lit5],
    });

    let inner_join = bld.push(CoreFrame::Join {
        label: j2,
        params: vec![y],
        rhs: jump_j1,
        body: jump_j2,
    });
    bld.push(CoreFrame::Join {
        label: j1,
        params: vec![x],
        rhs: add,
        body: inner_join,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 11);
    }
}
