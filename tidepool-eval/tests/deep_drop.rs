//! Regression: deep `Value` spines must drop without overflowing the host
//! thread stack.
//!
//! `Value::Drop` (tidepool-eval/src/value.rs) flattens `Con`/`ConFun` field
//! spines into an explicit worklist before the compiler-generated field drop
//! runs, so each link drops shallow. The auto-derived (recursive) drop costs
//! ~3 stack frames per nested link; effect responses and eval results can be
//! cons-spines tens of thousands deep, so a recursive drop SIGSEGVs the host
//! thread — a crash OUTSIDE the JIT signal handler that presents as a silent
//! thread death (the "host stack-overflow" class).
//!
//! These tests build spines FAR past any reasonable stack budget (>= 1M links)
//! and drop them on a deliberately small (512 KiB) thread, asserting clean
//! completion. With the iterative drop they pass; a regression to a recursive
//! drop would abort the test binary on stack overflow (loud, as intended).

use tidepool_eval::value::Value;
use tidepool_repr::{DataConId, Literal};

/// Links well past the ~5K frames a 512 KiB stack affords a recursive drop.
const SPINE_DEPTH: usize = 1_000_000;
/// Small enough that a recursive (per-link) drop is guaranteed to overflow.
const SMALL_STACK: usize = 512 * 1024;

/// Run `f` (which drops a deep spine) on a small-stack thread and require it to
/// finish. A stack overflow inside `f` aborts the process; a clean iterative
/// drop returns normally.
fn drop_on_small_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(f)
        .expect("spawn small-stack thread")
        .join()
        .expect("dropping a deep Value spine overflowed the host thread stack");
}

/// Unary-constructor spine: `Con(Con(...Con(Lit)))` — recursion through the
/// single field slot.
fn build_unary_con_spine(depth: usize) -> Value {
    let mut v = Value::Lit(Literal::LitInt(0));
    for _ in 0..depth {
        v = Value::Con(DataConId(1), vec![v]);
    }
    v
}

/// Haskell cons-list shape: `Cons(elem, Cons(elem, ... Nil))` — recursion
/// through the SECOND field (the realistic effect-result / list spine).
fn build_cons_list(depth: usize) -> Value {
    let mut v = Value::Con(DataConId(0), vec![]); // Nil
    for i in 0..depth {
        let head = Value::Lit(Literal::LitInt(i as i64));
        v = Value::Con(DataConId(1), vec![head, v]); // Cons head tail
    }
    v
}

/// Partial-application constructor spine: exercises the `ConFun` arm of the
/// drop worklist.
fn build_confun_spine(depth: usize) -> Value {
    let mut v = Value::Lit(Literal::LitInt(0));
    for _ in 0..depth {
        v = Value::ConFun(DataConId(1), 1, vec![v]);
    }
    v
}

#[test]
fn deep_unary_con_spine_drops_on_small_stack() {
    let v = build_unary_con_spine(SPINE_DEPTH);
    drop_on_small_stack(move || drop(v));
}

#[test]
fn deep_cons_list_drops_on_small_stack() {
    let v = build_cons_list(SPINE_DEPTH);
    drop_on_small_stack(move || drop(v));
}

#[test]
fn deep_confun_spine_drops_on_small_stack() {
    let v = build_confun_spine(SPINE_DEPTH);
    drop_on_small_stack(move || drop(v));
}

/// Mixed shape: a cons-list whose elements are themselves unary-con towers.
/// Both the outer tail spine and the inner field spines must flatten.
#[test]
fn deep_mixed_spine_drops_on_small_stack() {
    let mut v = Value::Con(DataConId(0), vec![]); // Nil
    for _ in 0..2_000 {
        let elem = build_unary_con_spine(2_000);
        v = Value::Con(DataConId(1), vec![elem, v]);
    }
    drop_on_small_stack(move || drop(v));
}
