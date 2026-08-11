//! Stack-safety tests for the hylomorphism-based codegen.
//!
//! These tests construct deep expression trees that would overflow the Rust
//! call stack under naive recursive tree-walking, and verify they compile
//! and execute correctly via the `recursion` crate's explicit stack.

use tidepool_heap::layout;
use tidepool_repr::*;
use tidepool_testing::jit_run::{compile_and_run, read_lit_int};

const NURSERY: usize = 1 << 20;

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
fn build_list(values: &[i64]) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    // Start with Nil
    let nil_idx = nodes.len();
    nodes.push(CoreFrame::Con {
        tag: NIL_TAG,
        fields: vec![],
    });

    let mut tail = nil_idx;
    for &v in values.iter().rev() {
        let lit_idx = nodes.len();
        nodes.push(CoreFrame::Lit(Literal::LitInt(v)));
        let cons_idx = nodes.len();
        nodes.push(CoreFrame::Con {
            tag: CONS_TAG,
            fields: vec![lit_idx, tail],
        });
        tail = cons_idx;
    }

    RecursiveTree { nodes }
}

/// Build a deep chain of PrimOp(IntAdd, [prev, 1]) with `depth` additions.
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
        nodes.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![acc, one_idx],
        });
        acc = add_idx;
    }

    RecursiveTree { nodes }
}

/// Build nested identity-function applications:
/// (λx.x) ((λx.x) ((λx.x) (... 42)))
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
        nodes.push(CoreFrame::Lam {
            binder: var_id,
            body: var_idx,
        });
        let app_idx = nodes.len();
        nodes.push(CoreFrame::App {
            fun: lam_idx,
            arg: inner,
        });
        inner = app_idx;
    }

    RecursiveTree { nodes }
}

/// Build a deeply case-nested expression:
/// `case 0 of { _ -> case 0 of { _ -> ... Lit(42) } }`, `depth` cases deep.
///
/// A tail Case's alt body re-enters `emit_node` natively (emit_node →
/// emit_case → emit_node), growing the call stack ~one frame per case
/// level — the cliff `stacker::maybe_grow` at `emit_node` guards. The shared
/// `Lit(0)` scrutinee keeps the IR small so Cranelift's own
/// (IR-size-proportional) passes stay well within the test stack — isolating
/// the emit-recursion depth.
fn build_deep_case_chain(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();
    let scrut = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(0)));
    let mut inner = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(42)));
    for i in 0..depth {
        let case = nodes.len();
        nodes.push(CoreFrame::Case {
            scrutinee: scrut,
            binder: VarId(1_000_000 + i as u64),
            alts: vec![Alt {
                con: AltCon::Default,
                binders: vec![],
                body: inner,
            }],
        });
        inner = case;
    }
    RecursiveTree { nodes }
}

/// Build a deep chain of Con nodes: Con(tag, [Con(tag, [... Lit(42)])])
fn build_deep_con_chain(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();

    let lit_idx = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(42)));

    let mut inner = lit_idx;
    for i in 0..depth {
        let con_idx = nodes.len();
        nodes.push(CoreFrame::Con {
            tag: DataConId(100 + i as u64),
            fields: vec![inner],
        });
        inner = con_idx;
    }

    RecursiveTree { nodes }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 200-element list: produces ~400 Con nodes (Cons + Lit pairs).
#[test]
fn test_deep_list_200() {
    let values: Vec<i64> = (1..=200).collect();
    let tree = build_list(&values);
    assert!(
        tree.nodes.len() > 400,
        "tree should have >400 nodes, got {}",
        tree.nodes.len()
    );

    let result = compile_and_run(&tree, NURSERY);
    unsafe {
        // Result is Cons(1, Cons(2, ...))
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        assert_eq!(read_con_tag(result.result_ptr), CONS_TAG.0);
        // First element should be 1
        let head = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(head), 1);
    }
}

#[test]
fn test_deep_list_500() {
    let values: Vec<i64> = (1..=500).collect();
    let tree = build_list(&values);
    assert!(tree.nodes.len() > 1000);

    let result = compile_and_run(&tree, NURSERY);
    unsafe {
        let head = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(head), 1);
    }
}

#[test]
fn test_deep_add_chain_500() {
    let tree = build_deep_add_chain(500);
    assert!(tree.nodes.len() > 1000);

    let result = compile_and_run(&tree, NURSERY);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 500);
    }
}

#[test]
fn test_deep_add_chain_1000() {
    let tree = build_deep_add_chain(1000);
    assert!(tree.nodes.len() > 2000);

    let result = compile_and_run(&tree, NURSERY);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 1000);
    }
}

#[test]
fn test_deep_app_chain_200() {
    let tree = build_deep_app_chain(200);
    assert!(tree.nodes.len() > 600);

    let result = compile_and_run(&tree, NURSERY);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}

#[test]
fn test_deep_con_chain_200() {
    let tree = build_deep_con_chain(200);
    assert!(tree.nodes.len() > 200);

    let result = compile_and_run(&tree, NURSERY);
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
    nodes.push(CoreFrame::Con {
        tag: NIL_TAG,
        fields: vec![],
    });

    let mut tail = nil_idx;
    for i in (0..n).rev() {
        let var_idx = nodes.len();
        nodes.push(CoreFrame::Var(VarId(i as u64)));
        let cons_idx = nodes.len();
        nodes.push(CoreFrame::Con {
            tag: CONS_TAG,
            fields: vec![var_idx, tail],
        });
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
    let result = compile_and_run(&tree, NURSERY);
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

    // 2MB: the hylomorphism keeps tree-walking off the host stack, while still
    // giving Cranelift room for its internal passes on ~4000 IR instructions.
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree, NURSERY);
            unsafe { read_lit_int(r.result_ptr) }
        })
        .unwrap()
        .join()
        .expect("thread panicked — stack overflow?");

    assert_eq!(result, 2000);
}

#[test]
fn test_deep_list_small_stack() {
    let values: Vec<i64> = (1..=500).collect();
    let tree = build_list(&values);

    let result = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree, NURSERY);
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

/// Stress test: deep case nesting on a 1 MiB stack.
///
/// Case-alt bodies re-enter `emit_node` natively, so without the
/// `stacker::maybe_grow` guard at `emit_node` this overflows (debug emit frames
/// are ~tens of KiB, so ~250 levels far exceed 1 MiB). With the guard, emit
/// transparently grows onto fresh 4 MiB segments and compiles. The IR is tiny
/// (one shared scrutinee + `depth` Default cases), so Cranelift's own passes
/// stay well within the stack — what's exercised here is the emit recursion.
#[test]
fn test_deep_case_nesting_small_stack() {
    let depth = 250;
    let tree = build_deep_case_chain(depth);

    let result = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree, NURSERY);
            unsafe { read_lit_int(r.result_ptr) }
        })
        .unwrap()
        .join()
        .expect("thread panicked — emit recursion overflowed the stack?");

    // Every case takes its Default alt down to the innermost Lit(42).
    assert_eq!(result, 42);
}

#[test]
fn test_deep_app_small_stack() {
    let tree = build_deep_app_chain(200);

    let result = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let r = compile_and_run(&tree, NURSERY);
            unsafe { read_lit_int(r.result_ptr) }
        })
        .unwrap()
        .join()
        .expect("thread panicked — stack overflow?");

    assert_eq!(result, 42);
}
