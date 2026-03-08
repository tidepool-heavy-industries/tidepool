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
fn test_letrec_all_simple() {
    // letrec x = 1; y = x + 1; z = y + 1 in z
    let x = VarId(1);
    let y = VarId(2);
    let z = VarId(3);

    let mut bld = TreeBuilder::new();
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    
    let vx = bld.push(CoreFrame::Var(x));
    let add1 = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![vx, lit1] });
    
    let vy = bld.push(CoreFrame::Var(y));
    let add2 = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![vy, lit1] });
    
    let vz = bld.push(CoreFrame::Var(z));
    
    bld.push(CoreFrame::LetRec {
        bindings: vec![(x, lit1), (y, add1), (z, add2)],
        body: vz,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 3);
    }
}

#[test]
fn test_letrec_factorial() {
    // letrec f = \n -> if n == 0 then 1 else n * f(n-1) in f(5)
    let f = VarId(1);
    let n = VarId(2);
    let binder = VarId(3);

    let mut bld = TreeBuilder::new();
    
    // Lambda body: case n == 0 of { 1 -> 1; _ -> n * f(n-1) }
    let vn = bld.push(CoreFrame::Var(n));
    let lit0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntEq, args: vec![vn, lit0] });
    
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    
    let vn2 = bld.push(CoreFrame::Var(n));
    let vf = bld.push(CoreFrame::Var(f));
    let sub1 = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntSub, args: vec![vn2, lit1] });
    let call = bld.push(CoreFrame::App { fun: vf, arg: sub1 });
    let vn3 = bld.push(CoreFrame::Var(n));
    let mul = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![vn3, call] });
    
    let case_node = bld.push(CoreFrame::Case {
        scrutinee: cmp,
        binder,
        alts: vec![
            Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: lit1 },
            Alt { con: AltCon::Default, binders: vec![], body: mul },
        ],
    });
    
    let lam = bld.push(CoreFrame::Lam { binder: n, body: case_node });
    
    let lit5 = bld.push(CoreFrame::Lit(Literal::LitInt(5)));
    let vf2 = bld.push(CoreFrame::Var(f));
    let app = bld.push(CoreFrame::App { fun: vf2, arg: lit5 });
    
    bld.push(CoreFrame::LetRec {
        bindings: vec![(f, lam)],
        body: app,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 120);
    }
}

#[test]
fn test_letrec_cyclic_con() {
    // Build two Con objects that reference each other.
    // letrec 
    //   a = Con(0, [b])
    //   b = Con(0, [a])
    // in a
    let a = VarId(1);
    let b = VarId(2);
    let tag = DataConId(0);

    let mut bld = TreeBuilder::new();
    let va = bld.push(CoreFrame::Var(a));
    let vb = bld.push(CoreFrame::Var(b));
    
    let con_a = bld.push(CoreFrame::Con { tag, fields: vec![vb] });
    let con_b = bld.push(CoreFrame::Con { tag, fields: vec![va] });
    
    bld.push(CoreFrame::LetRec {
        bindings: vec![(a, con_a), (b, con_b)],
        body: va,
    });
    
    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        let ptr_a = result.result_ptr;
        assert_eq!(layout::read_tag(ptr_a), layout::TAG_CON);
        let ptr_b = *(ptr_a.add(24) as *const *const u8);
        assert_eq!(layout::read_tag(ptr_b), layout::TAG_CON);
        let ptr_a_back = *(ptr_b.add(24) as *const *const u8);
        assert_eq!(ptr_a, ptr_a_back);
    }
}

#[test]
fn test_letrec_deferred_simple() {
    // letrec
    //   f = \n -> if n == 0 then 1 else n * f(n-1)
    //   x = f(5)
    // in x
    let f = VarId(1);
    let n = VarId(2);
    let binder = VarId(3);
    let x = VarId(4);

    let mut bld = TreeBuilder::new();
    
    // Lambda f
    let vn = bld.push(CoreFrame::Var(n));
    let lit0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntEq, args: vec![vn, lit0] });
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let vn2 = bld.push(CoreFrame::Var(n));
    let vf = bld.push(CoreFrame::Var(f));
    let sub1 = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntSub, args: vec![vn2, lit1] });
    let call = bld.push(CoreFrame::App { fun: vf, arg: sub1 });
    let vn3 = bld.push(CoreFrame::Var(n));
    let mul = bld.push(CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![vn3, call] });
    let case_node = bld.push(CoreFrame::Case {
        scrutinee: cmp,
        binder,
        alts: vec![
            Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: lit1 },
            Alt { con: AltCon::Default, binders: vec![], body: mul },
        ],
    });
    let lam = bld.push(CoreFrame::Lam { binder: n, body: case_node });
    
    // x = f(5)
    let lit5 = bld.push(CoreFrame::Lit(Literal::LitInt(5)));
    let vf2 = bld.push(CoreFrame::Var(f));
    let app = bld.push(CoreFrame::App { fun: vf2, arg: lit5 });
    
    let vx = bld.push(CoreFrame::Var(x));
    
    bld.push(CoreFrame::LetRec {
        bindings: vec![(f, lam), (x, app)],
        body: vx,
    });

    let tree = bld.build();
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 120);
    }
}
