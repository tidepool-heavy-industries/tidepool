//! Stack-safety tests for the hylomorphism-based codegen.
//!
//! These tests construct deep expression trees that would overflow the Rust
//! call stack under naive recursive tree-walking, and verify they compile
//! and execute correctly via the `recursion` crate's explicit stack.

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

fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 1 << 20]; // 1 MiB nursery for deep trees
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

unsafe fn read_con_tag(ptr: *const u8) -> u64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_CON);
    *(ptr.add(8) as *const u64)
}

unsafe fn read_con_field(ptr: *const u8, i: usize) -> *const u8 {
    *(ptr.add(24 + 8 * i) as *const *const u8)
}

// ---------------------------------------------------------------------------
// Helpers: build deep trees
// ---------------------------------------------------------------------------

const NIL_TAG: DataConId = DataConId(0);
const CONS_TAG: DataConId = DataConId(1);

/// Build a Haskell-style list: Cons(x, Cons(y, ... Nil))
/// Returns the tree. Root is the last node.
fn build_list(values: &[i64]) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    // Start with Nil
    let nil_idx = nodes.len();
    nodes.push(CoreFrame::Con { tag: NIL_TAG, fields: vec![] });

    let mut tail = nil_idx;
    for &v in values.iter().rev() {
        let lit_idx = nodes.len();
        nodes.push(CoreFrame::Lit(Literal::LitInt(v)));
        let cons_idx = nodes.len();
        nodes.push(CoreFrame::Con { tag: CONS_TAG, fields: vec![lit_idx, tail] });
        tail = cons_idx;
    }

    RecursiveTree { nodes }
}

/// Build a deep chain of PrimOp(IntAdd, [prev, 1]):
/// ((((0 + 1) + 1) + 1) + ... + 1) with `depth` additions.
fn build_deep_add_chain(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    // Accumulator starts at 0
    let zero_idx = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(0)));

    let mut acc = zero_idx;
    for _ in 0..depth {
        let one_idx = nodes.len();
        nodes.push(CoreFrame::Lit(Literal::LitInt(1)));
        let add_idx = nodes.len();
        nodes.push(CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![acc, one_idx] });
        acc = add_idx;
    }

    RecursiveTree { nodes }
}

/// Build a deep chain of App(App(App(..., arg), arg), arg).
/// f(x)(x)(x)... where f = λa.λb.λc....a (returns first arg).
/// We'll use a simpler approach: nested identity applications.
/// (λx.x) ((λx.x) ((λx.x) (... 42)))
/// Each application wraps an identity function around the previous result.
fn build_deep_app_chain(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    // The innermost value: Lit(42)
    let lit_idx = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(42)));

    let mut inner = lit_idx;
    // Each identity function uses a unique VarId
    for i in 0..depth {
        let var_id = VarId(1000 + i as u64);
        let var_idx = nodes.len();
        nodes.push(CoreFrame::Var(var_id));
        let lam_idx = nodes.len();
        nodes.push(CoreFrame::Lam { binder: var_id, body: var_idx });
        let app_idx = nodes.len();
        nodes.push(CoreFrame::App { fun: lam_idx, arg: inner });
        inner = app_idx;
    }

    RecursiveTree { nodes }
}

/// Build a deep chain of Con nodes: Con(tag, [Con(tag, [... Lit(42)])])
/// Nested unary constructors.
fn build_deep_con_chain(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    let lit_idx = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(42)));

    let mut inner = lit_idx;
    for i in 0..depth {
        let con_idx = nodes.len();
        nodes.push(CoreFrame::Con { tag: DataConId(100 + i as u64), fields: vec![inner] });
        inner = con_idx;
    }

    RecursiveTree { nodes }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 200-element list: produces ~400 Con nodes (Cons + Lit pairs).
/// Would overflow a ~2MB stack with recursive emit_node (~20 bytes/frame × 400 = 8KB,
/// but the actual per-frame cost is higher due to Cranelift builder state).
#[test]
fn test_deep_list_200() {
    let values: Vec<i64> = (1..=200).collect();
    let tree = build_list(&values);
    assert!(tree.nodes.len() > 400, "tree should have >400 nodes, got {}", tree.nodes.len());

    let result = compile_and_run(&tree);
    unsafe {
        // Result is Cons(1, Cons(2, ...))
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        assert_eq!(read_con_tag(result.result_ptr), CONS_TAG.0);
        // First element should be 1
        let head = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(head), 1);
    }
}

/// 500-element list: 1000+ nodes.
#[test]
fn test_deep_list_500() {
    let values: Vec<i64> = (1..=500).collect();
    let tree = build_list(&values);
    assert!(tree.nodes.len() > 1000);

    let result = compile_and_run(&tree);
    unsafe {
        let head = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(head), 1);
    }
}

/// 500 nested additions: 0 + 1 + 1 + ... + 1 = 500.
/// Creates a 1001-node tree (500 PrimOp + 501 Lit).
#[test]
fn test_deep_add_chain_500() {
    let tree = build_deep_add_chain(500);
    assert!(tree.nodes.len() > 1000);

    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 500); }
}

/// 1000 nested additions.
#[test]
fn test_deep_add_chain_1000() {
    let tree = build_deep_add_chain(1000);
    assert!(tree.nodes.len() > 2000);

    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 1000); }
}

/// 200 nested identity applications: (λx.x) ((λx.x) (... 42)) = 42.
/// Each application adds 3 nodes (Var, Lam, App), so 600+ nodes.
#[test]
fn test_deep_app_chain_200() {
    let tree = build_deep_app_chain(200);
    assert!(tree.nodes.len() > 600);

    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

/// 200 nested unary constructors wrapping a Lit(42).
#[test]
fn test_deep_con_chain_200() {
    let tree = build_deep_con_chain(200);
    assert!(tree.nodes.len() > 200);

    let result = compile_and_run(&tree);
    unsafe {
        // Outermost constructor
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        assert_eq!(read_con_tag(result.result_ptr), 100 + 199); // last tag
        // Dig down to innermost
        let mut ptr = result.result_ptr;
        for i in (0..200).rev() {
            assert_eq!(read_con_tag(ptr), 100 + i as u64);
            ptr = read_con_field(ptr, 0);
        }
        assert_eq!(read_lit_int(ptr), 42);
    }
}

/// Mixed: deep list inside a let-chain.
/// let x0 = Lit(0) in let x1 = Lit(1) in ... let xN = Lit(N) in [x0, x1, ..., xN]
/// Tests interaction between iterative let-loop and hylomorphism.
#[test]
fn test_let_chain_then_deep_list() {
    let n = 100;
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    // First, push all the Lit nodes that will be let-bound
    let mut lit_indices = Vec::new();
    for i in 0..n {
        let idx = nodes.len();
        nodes.push(CoreFrame::Lit(Literal::LitInt(i as i64)));
        lit_indices.push(idx);
    }

    // Build the list body: Cons(x0, Cons(x1, ... Nil))
    let nil_idx = nodes.len();
    nodes.push(CoreFrame::Con { tag: NIL_TAG, fields: vec![] });

    let mut tail = nil_idx;
    for i in (0..n).rev() {
        let var_idx = nodes.len();
        nodes.push(CoreFrame::Var(VarId(i as u64)));
        let cons_idx = nodes.len();
        nodes.push(CoreFrame::Con { tag: CONS_TAG, fields: vec![var_idx, tail] });
        tail = cons_idx;
    }

    // Wrap in let-chain: let x0 = 0 in let x1 = 1 in ... in list
    let mut body = tail;
    for i in (0..n).rev() {
        let let_idx = nodes.len();
        nodes.push(CoreFrame::LetNonRec {
            binder: VarId(i as u64),
            rhs: lit_indices[i],
            body,
        });
        body = let_idx;
    }

    let tree = RecursiveTree { nodes };
    let result = compile_and_run(&tree);
    unsafe {
        // First element should be 0
        let head = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(head), 0);
        // Second element
        let tail = read_con_field(result.result_ptr, 1);
        let head2 = read_con_field(tail, 0);
        assert_eq!(read_lit_int(head2), 1);
    }
}

/// Stress test: 2000 nested PrimOps with a restricted stack.
/// Uses 2MB — the hylomorphism itself is heap-based, but Cranelift's internal
/// regalloc/isel passes still use call-stack proportional to IR size.
#[test]
fn test_deep_add_small_stack() {
    let tree = build_deep_add_chain(2000);

    // 2MB: proves our tree-walking is stack-safe (was overflowing before hylomorphism),
    // while giving Cranelift enough room for its internal passes on ~4000 IR instructions.
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree);
            unsafe { read_lit_int(r.result_ptr) }
        })
        .unwrap()
        .join()
        .expect("thread panicked — stack overflow?");

    assert_eq!(result, 2000);
}

/// Stress test: 500-element list on a 512KB stack.
#[test]
fn test_deep_list_small_stack() {
    let values: Vec<i64> = (1..=500).collect();
    let tree = build_list(&values);

    let result = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree);
            unsafe {
                let head = read_con_field(r.result_ptr, 0);
                read_lit_int(head)
            }
        })
        .unwrap()
        .join()
        .expect("thread panicked — stack overflow?");

    assert_eq!(result, 1);
}

/// Stress test: 200 nested App on a 512KB stack.
#[test]
fn test_deep_app_small_stack() {
    let tree = build_deep_app_chain(200);

    let result = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree);
            unsafe { read_lit_int(r.result_ptr) }
        })
        .unwrap()
        .join()
        .expect("thread panicked — stack overflow?");

    assert_eq!(result, 42);
}
