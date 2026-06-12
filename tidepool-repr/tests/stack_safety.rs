//! Stack-safety tests for the structural tree walks (`extract_subtree`,
//! `replace_subtree`, `free_vars`).
//!
//! These walks run in the JIT emit hot path over the same deep trees that
//! produced the emit stack cliff. Each was converted from native recursion to
//! an explicit-stack post-order walk; these tests build trees far deeper than a
//! small thread's call budget and run each walk on a 256 KiB thread, asserting
//! clean completion AND correct results. Under the former recursive walks these
//! overflowed (process abort); the explicit-stack versions complete.

use tidepool_repr::free_vars::free_vars;
use tidepool_repr::{replace_subtree, CoreExpr, CoreFrame, Literal, RecursiveTree, VarId};

/// Depth well past what a 256 KiB stack affords a recursive per-level walk
/// (hundreds of bytes/frame ⇒ a few thousand frames max).
const DEPTH: usize = 200_000;
const SMALL_STACK: usize = 256 * 1024;

const F: VarId = VarId(1);
const X: VarId = VarId(2);

/// Right-nested application spine:
/// `App(Var f, App(Var f, ... App(Var f, Var x)))`, `depth` applications deep.
/// `Var f` (node 0) is SHARED by every App, so the walk's DAG handling is
/// exercised alongside its depth handling.
fn deep_app_spine(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();
    nodes.push(CoreFrame::Var(F)); // 0: shared fun
    nodes.push(CoreFrame::Var(X)); // 1: innermost arg
    let mut inner = 1;
    for _ in 0..depth {
        let app = nodes.len();
        nodes.push(CoreFrame::App { fun: 0, arg: inner });
        inner = app;
    }
    RecursiveTree { nodes }
}

fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(f)
        .expect("spawn small-stack thread")
        .join()
        .expect("a deep tree walk overflowed the host thread stack")
}

#[test]
fn extract_subtree_deep_is_stack_safe() {
    let tree = deep_app_spine(DEPTH);
    let expected_len = tree.nodes.len();
    let extracted = on_small_stack(move || {
        let root = tree.nodes.len() - 1;
        tree.extract_subtree(root)
    });
    // Whole tree reachable from root ⇒ identical node count (sharing preserved:
    // the single shared `Var f` is emitted once).
    assert_eq!(extracted.nodes.len(), expected_len);
    // Root stays an App; structural integrity intact.
    assert!(matches!(
        extracted.nodes[extracted.nodes.len() - 1],
        CoreFrame::App { .. }
    ));
}

#[test]
fn free_vars_deep_is_stack_safe() {
    let tree = deep_app_spine(DEPTH);
    let fvs = on_small_stack(move || free_vars(&tree));
    // f and x are the only free variables, however deep the spine.
    assert_eq!(fvs, vec![F, X]);
}

#[test]
fn replace_subtree_deep_is_stack_safe() {
    let tree = deep_app_spine(DEPTH);
    // Replace the innermost arg `Var x` (node 1) with a literal.
    let replacement = RecursiveTree {
        nodes: vec![CoreFrame::Lit(Literal::LitInt(99))],
    };
    let replaced = on_small_stack(move || replace_subtree(&tree, 1, &replacement));
    // Same shape, but the deepest leaf is now the literal. Walk down the arg
    // chain iteratively (the result tree is just as deep) to verify.
    let mut idx = replaced.nodes.len() - 1;
    loop {
        match &replaced.nodes[idx] {
            CoreFrame::App { arg, .. } => idx = *arg,
            CoreFrame::Lit(Literal::LitInt(n)) => {
                assert_eq!(*n, 99);
                break;
            }
            other => panic!("unexpected node at spine bottom: {other:?}"),
        }
    }
    // No stray `Var x` remains.
    assert!(!replaced
        .nodes
        .iter()
        .any(|n| matches!(n, CoreFrame::Var(v) if *v == X)));
}
