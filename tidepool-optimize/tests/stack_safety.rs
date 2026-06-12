//! Stack-safety tests for the rewrite passes' redex search.
//!
//! Each pass's `run` walks the whole tree looking for the first redex. That
//! search was converted from native recursion (`try_*_at`, depth = tree depth)
//! to an explicit heap stack (`rewrite::find_redex`). These tests build a tree
//! far deeper than a small thread's call budget and run every pass on a 256 KiB
//! thread, asserting clean completion. Under the former recursive search these
//! overflowed (process abort); the explicit-stack search completes.
//!
//! The spine is a redex-free `App(Var f, App(Var f, ... Var x))` tower: no
//! `Lam`/`Let`/`Case`, so every pass walks the full depth and reports no change
//! — isolating the SEARCH walk (the part converted here) from the rewrite step
//! (`subst`, deliberately left recursive — see subst.rs).

// PartialEval is intentionally absent: like `subst`, it threads a per-path
// environment and is left native-recursive (see partial.rs rationale), so it is
// NOT stack-safe on a tower this deep and is not part of this conversion.
use tidepool_eval::Pass;
use tidepool_optimize::{beta::BetaReduce, case_reduce::CaseReduce, dce::Dce, inline::Inline};
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree, VarId};

const DEPTH: usize = 200_000;
const SMALL_STACK: usize = 256 * 1024;

fn deep_app_spine(depth: usize) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = Vec::new();
    nodes.push(CoreFrame::Var(VarId(1))); // 0: shared fun (never a Lam ⇒ no beta redex)
    nodes.push(CoreFrame::Var(VarId(2))); // 1: innermost arg
    let mut inner = 1;
    for _ in 0..depth {
        let app = nodes.len();
        nodes.push(CoreFrame::App { fun: 0, arg: inner });
        inner = app;
    }
    RecursiveTree { nodes }
}

/// Run `pass.run` on a deep spine on a small stack; require completion and
/// (since the spine has no redex of any kind) no change.
fn assert_pass_stack_safe(pass: impl Pass + Send + 'static) {
    let changed = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let mut expr = deep_app_spine(DEPTH);
            pass.run(&mut expr)
        })
        .expect("spawn small-stack thread")
        .join()
        .expect("a rewrite pass's redex search overflowed the host thread stack");
    assert!(!changed, "redex-free spine should report no change");
}

#[test]
fn beta_search_deep_is_stack_safe() {
    assert_pass_stack_safe(BetaReduce);
}

#[test]
fn dce_search_deep_is_stack_safe() {
    assert_pass_stack_safe(Dce);
}

#[test]
fn inline_search_deep_is_stack_safe() {
    assert_pass_stack_safe(Inline);
}

#[test]
fn case_reduce_search_deep_is_stack_safe() {
    assert_pass_stack_safe(CaseReduce);
}
