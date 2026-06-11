//! Bug-hunting proptest suite for ParkedStream registry and stream machinery.
//!
//! Route: REAL code in `host_fns.rs` (~1989-2499) driven by Track A (unit `heap_force`)
//! and Track B (`JitEffectMachine` over hand-built Core trees).
//!
//! Bug Classes:
//! - B1: Model mismatch (incorrect values/spine).
//! - B2: Unexpected or missing error (e.g. signal instead of error, or Ok-with-garbage).
//! - B3: Fatal signal (SIGSEGV/SIGILL/SIGBUS) — mostly hunted in P7 (forked GC).
//! - B5: Memoization/isolation violation (stale state, double-pull).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use proptest::prelude::*;

use tidepool_codegen::context::VMContext;
use tidepool_codegen::host_fns;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_effect::dispatch::{EffectContext, EffectHandler, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::{ValueSource, ValueStream};
use tidepool_eval::value::Value;
use tidepool_heap::layout;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::types::{Alt, AltCon, DataConId, JoinId, Literal, PrimOpKind, VarId};
use tidepool_repr::DataCon;
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree};
use tidepool_testing::proptest::values_equal;

// ---------------------------------------------------------------------------
// Constants & IDs matching make_table()
// ---------------------------------------------------------------------------

const VAL: DataConId = DataConId(1);
const E: DataConId = DataConId(2);
const LEAF: DataConId = DataConId(3);
const UNION: DataConId = DataConId(5);
const CONS: DataConId = DataConId(6);
const NIL: DataConId = DataConId(7);

fn make_table() -> DataConTable {
    let mut t = DataConTable::new();
    for (id, name, tag, ar) in [
        (VAL, "Val", 1, 1),
        (E, "E", 2, 2),
        (LEAF, "Leaf", 1, 1),
        (DataConId(4), "Node", 2, 2),
        (UNION, "Union", 1, 2),
        (CONS, ":", 2, 2),
        (NIL, "[]", 1, 0),
    ] {
        t.insert(DataCon {
            id,
            name: name.into(),
            tag,
            rep_arity: ar,
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    t
}

// ---------------------------------------------------------------------------
// Track A — no Cranelift: heap_force unit properties
// ---------------------------------------------------------------------------

thread_local! {
    static TRACK_A_COUNTER: Cell<usize> = const { Cell::new(0) };
}

extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    assert_eq!(
        *(ptr.add(layout::LIT_TAG_OFFSET)),
        layout::LitTag::Int as u8
    );
    *(ptr.add(layout::LIT_VALUE_OFFSET) as *const i64)
}

#[test]
fn t_a1_memoization() {
    extern "C" fn entry(_vmctx: *mut VMContext, _thunk: *mut u8) -> *mut u8 {
        TRACK_A_COUNTER.with(|c| c.set(c.get() + 1));
        unsafe {
            let p = _thunk.add(1024);
            layout::write_header(p, layout::TAG_LIT, layout::LIT_SIZE as u16);
            *(p.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
            *(p.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 42;
            p
        }
    }

    TRACK_A_COUNTER.with(|c| c.set(0));
    unsafe {
        let mut nursery = vec![0u8; 4096];
        let start = nursery.as_mut_ptr();
        let end = start.add(nursery.len());
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);
        host_fns::set_gc_state(start, nursery.len());

        let thunk_ptr = start;
        layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
        *(thunk_ptr.add(layout::THUNK_STATE_OFFSET)) = layout::THUNK_UNEVALUATED;
        *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET) as *mut usize) = entry as *const () as usize;

        let res1 = host_fns::heap_force(&mut vmctx, thunk_ptr);
        assert_eq!(read_lit_int(res1), 42);
        assert_eq!(TRACK_A_COUNTER.with(|c| c.get()), 1);
        assert_eq!(
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET)),
            layout::THUNK_EVALUATED
        );

        let res2 = host_fns::heap_force(&mut vmctx, thunk_ptr);
        assert_eq!(res1, res2);
        assert_eq!(
            TRACK_A_COUNTER.with(|c| c.get()),
            1,
            "Memoization failed: entry called twice"
        );
        host_fns::clear_gc_state();
    }
}

#[test]
fn t_a2_poison_memoization() {
    extern "C" fn entry_poison(_vmctx: *mut VMContext, _thunk: *mut u8) -> *mut u8 {
        let msg = b"test error";
        host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64)
    }

    unsafe {
        let mut nursery = vec![0u8; 4096];
        let start = nursery.as_mut_ptr();
        let end = start.add(nursery.len());
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);
        host_fns::set_gc_state(start, nursery.len());
        let _ = host_fns::take_runtime_error();

        let thunk_ptr = start;
        layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
        *(thunk_ptr.add(layout::THUNK_STATE_OFFSET)) = layout::THUNK_UNEVALUATED;
        *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET) as *mut usize) =
            entry_poison as *const () as usize;

        let res1 = host_fns::heap_force(&mut vmctx, thunk_ptr);
        assert_eq!(
            res1,
            host_fns::error_poison_ptr(),
            "First force must return poison"
        );
        assert!(
            host_fns::take_runtime_error().is_some(),
            "First force must set runtime error"
        );

        let res2 = host_fns::heap_force(&mut vmctx, thunk_ptr);
        assert_eq!(res1, res2, "Second force must return SAME memoized poison");
        assert!(
            host_fns::take_runtime_error().is_none(),
            "Second force must NOT set error (already consumed)"
        );
        host_fns::clear_gc_state();
    }
}

#[test]
fn t_a3_reentrant_blackhole() {
    extern "C" fn entry_reentrant(vmctx: *mut VMContext, thunk: *mut u8) -> *mut u8 {
        host_fns::heap_force(vmctx, thunk)
    }

    unsafe {
        let mut nursery = vec![0u8; 4096];
        let start = nursery.as_mut_ptr();
        let end = start.add(nursery.len());
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);
        host_fns::set_gc_state(start, nursery.len());
        let _ = host_fns::take_runtime_error();

        let thunk_ptr = start;
        layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
        *(thunk_ptr.add(layout::THUNK_STATE_OFFSET)) = layout::THUNK_UNEVALUATED;
        *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET) as *mut usize) =
            entry_reentrant as *const () as usize;

        let res = host_fns::heap_force(&mut vmctx, thunk_ptr);
        assert_eq!(
            res,
            host_fns::error_poison_ptr(),
            "Blackhole must return poison"
        );
        let err = host_fns::take_runtime_error().expect("Blackhole must set runtime error");
        assert!(
            format!("{}", err).contains("blackhole"),
            "Error msg should contain 'blackhole', got: {}",
            err
        );
        host_fns::clear_gc_state();
    }
}

#[test]
fn t_a4_indirection_chains() {
    unsafe {
        let mut nursery = vec![0u8; 4096];
        let start = nursery.as_mut_ptr();
        let end = start.add(nursery.len());
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);
        host_fns::set_gc_state(start, nursery.len());

        let lit_ptr = start;
        layout::write_header(lit_ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(lit_ptr.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit_ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 100;

        let thunk_b = start.add(32);
        layout::write_header(thunk_b, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
        *(thunk_b.add(layout::THUNK_STATE_OFFSET)) = layout::THUNK_EVALUATED;
        *(thunk_b.add(layout::THUNK_INDIRECTION_OFFSET) as *mut *mut u8) = lit_ptr;

        let thunk_a = start.add(64);
        layout::write_header(thunk_a, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
        *(thunk_a.add(layout::THUNK_STATE_OFFSET)) = layout::THUNK_EVALUATED;
        *(thunk_a.add(layout::THUNK_INDIRECTION_OFFSET) as *mut *mut u8) = thunk_b;

        let res = host_fns::heap_force(&mut vmctx, thunk_a);
        assert_eq!(res, lit_ptr, "Indirection chain should resolve to Lit");
        assert_eq!(read_lit_int(res), 100);
        host_fns::clear_gc_state();
    }
}

// ---------------------------------------------------------------------------
// Track B — Adversarial Source Zoo
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct Stats {
    next_calls: usize,
    get_calls: usize,
    len_calls: usize,
    dropped: bool,
    drop_order: Option<usize>,
}

static DROP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

struct InstrumentedSource<S> {
    inner: S,
    stats: Rc<RefCell<Stats>>,
}

impl<S: ValueSource> ValueSource for InstrumentedSource<S> {
    fn next_value(
        &mut self,
        table: &tidepool_repr::DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        self.stats.borrow_mut().next_calls += 1;
        self.inner.next_value(table)
    }
    fn len(&self) -> Option<usize> {
        self.stats.borrow_mut().len_calls += 1;
        self.inner.len()
    }
    fn get(
        &self,
        idx: usize,
        table: &tidepool_repr::DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        self.stats.borrow_mut().get_calls += 1;
        self.inner.get(idx, table)
    }
}

unsafe impl<S> Send for InstrumentedSource<S> {}

impl<S> Drop for InstrumentedSource<S> {
    fn drop(&mut self) {
        let mut s = self.stats.borrow_mut();
        s.dropped = true;
        s.drop_order = Some(DROP_COUNTER.fetch_add(1, Ordering::SeqCst));
    }
}

struct SeqSource {
    data: Vec<i64>,
    pos: usize,
}
impl ValueSource for SeqSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        let v = self.data.get(self.pos)?;
        self.pos += 1;
        Some(Ok(Value::Lit(Literal::LitInt(*v))))
    }
}

struct IdxSource {
    data: Vec<i64>,
    pos: usize,
}
impl ValueSource for IdxSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        let v = self.data.get(self.pos)?;
        self.pos += 1;
        Some(Ok(Value::Lit(Literal::LitInt(*v))))
    }
    fn len(&self) -> Option<usize> {
        Some(self.data.len())
    }
    fn get(
        &self,
        idx: usize,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        self.data
            .get(idx)
            .map(|v| Ok(Value::Lit(Literal::LitInt(*v))))
    }
}

struct PanicSeqSource {
    data: Vec<i64>,
    pos: usize,
    panic_at: usize,
}
impl ValueSource for PanicSeqSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        if self.pos == self.panic_at {
            panic!("test-producer-panic");
        }
        let v = self.data.get(self.pos)?;
        self.pos += 1;
        Some(Ok(Value::Lit(Literal::LitInt(*v))))
    }
}

struct LyingLenSource {
    actual: Vec<i64>,
    claimed: usize,
    pos: usize,
}
impl ValueSource for LyingLenSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        let v = self.actual.get(self.pos)?;
        self.pos += 1;
        Some(Ok(Value::Lit(Literal::LitInt(*v))))
    }
    fn len(&self) -> Option<usize> {
        Some(self.claimed)
    }
    fn get(
        &self,
        idx: usize,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        self.actual
            .get(idx)
            .map(|v| Ok(Value::Lit(Literal::LitInt(*v))))
    }
}

struct PanicIdxSource {
    data: Vec<i64>,
    panic_idx: usize,
    pos: usize,
}
impl ValueSource for PanicIdxSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        let v = self.data.get(self.pos)?;
        self.pos += 1;
        Some(Ok(Value::Lit(Literal::LitInt(*v))))
    }
    fn len(&self) -> Option<usize> {
        Some(self.data.len())
    }
    fn get(
        &self,
        idx: usize,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        if idx == self.panic_idx {
            panic!("test-element-panic");
        }
        self.data
            .get(idx)
            .map(|v| Ok(Value::Lit(Literal::LitInt(*v))))
    }
}

struct FailingConvSource {
    fail_at: usize,
    data: Vec<i64>,
    pos: usize,
    indexed: bool,
}
impl ValueSource for FailingConvSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        if !self.indexed && self.pos == self.fail_at {
            return Some(Err(tidepool_bridge::BridgeError::UnknownDataConName(
                "test-fail".into(),
            )));
        }
        let v = self.data.get(self.pos)?;
        self.pos += 1;
        Some(Ok(Value::Lit(Literal::LitInt(*v))))
    }
    fn len(&self) -> Option<usize> {
        if self.indexed {
            Some(self.data.len())
        } else {
            None
        }
    }
    fn get(
        &self,
        idx: usize,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        if self.indexed && idx == self.fail_at {
            return Some(Err(tidepool_bridge::BridgeError::UnknownDataConName(
                "test-fail".into(),
            )));
        }
        self.data
            .get(idx)
            .map(|v| Ok(Value::Lit(Literal::LitInt(*v))))
    }
}

struct InfiniteGuardedSource {
    served: usize,
    guard: usize,
}
impl ValueSource for InfiniteGuardedSource {
    fn next_value(
        &mut self,
        _table: &DataConTable,
    ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
        if self.served > self.guard {
            panic!("infinite-guarded-source-runaway");
        }
        let v = self.served;
        self.served += 1;
        Some(Ok(Value::Lit(Literal::LitInt(v as i64))))
    }
}

// ---------------------------------------------------------------------------
// Track B — Program Builders & Harness
// ---------------------------------------------------------------------------

fn push_node(nodes: &mut Vec<CoreFrame<usize>>, f: CoreFrame<usize>) -> usize {
    nodes.push(f);
    nodes.len() - 1
}

fn wrap_effect(
    nodes: &mut Vec<CoreFrame<usize>>,
    tag: u64,
    req: i64,
    binder: VarId,
    body: usize,
) -> usize {
    let lam = push_node(nodes, CoreFrame::Lam { binder, body });
    let leaf = push_node(
        nodes,
        CoreFrame::Con {
            tag: LEAF,
            fields: vec![lam],
        },
    );
    let req_n = push_node(nodes, CoreFrame::Lit(Literal::LitInt(req)));
    let tag_n = push_node(nodes, CoreFrame::Lit(Literal::LitWord(tag)));
    let union = push_node(
        nodes,
        CoreFrame::Con {
            tag: UNION,
            fields: vec![tag_n, req_n],
        },
    );
    push_node(
        nodes,
        CoreFrame::Con {
            tag: E,
            fields: vec![union, leaf],
        },
    )
}

fn build_sum_chain(k: usize, tag: u64) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];

    let m999 = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(-999)));
    let val_m999 = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![m999],
        },
    );

    let mut sum_acc = None;
    for i in 0..k {
        let vi = push_node(&mut nodes, CoreFrame::Var(VarId(200 + i as u64))); // h_i
        sum_acc = match sum_acc {
            None => Some(vi),
            Some(prev) => {
                let p = push_node(
                    &mut nodes,
                    CoreFrame::PrimOp {
                        op: PrimOpKind::IntAdd,
                        args: vec![prev, vi],
                    },
                );
                Some(p)
            }
        };
    }
    let final_val = match sum_acc {
        None => push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(0))),
        Some(s) => s,
    };
    let mut current_body = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![final_val],
        },
    );

    for i in (0..k).rev() {
        let t_prev = VarId(300 + i as u64); // t_i
        let h_i = VarId(200 + i as u64);
        let t_next = VarId(300 + (i + 1) as u64);

        let v_t_prev = push_node(&mut nodes, CoreFrame::Var(t_prev));
        current_body = push_node(
            &mut nodes,
            CoreFrame::Case {
                scrutinee: v_t_prev,
                binder: VarId(400 + i as u64),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(CONS),
                        binders: vec![h_i, t_next],
                        body: current_body,
                    },
                    Alt {
                        con: AltCon::DataAlt(NIL),
                        binders: vec![],
                        body: val_m999,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: val_m999,
                    },
                ],
            },
        );
    }

    let root = wrap_effect(&mut nodes, tag, 0, VarId(300), current_body);
    let _ = root;
    RecursiveTree { nodes }
}

fn build_spine_walk(k: usize, tag: u64) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];

    let m999 = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(-999)));
    let val_m999 = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![m999],
        },
    );

    let lit_k = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(k as i64)));
    let mut current_body = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![lit_k],
        },
    );

    for i in (0..k).rev() {
        let t_prev = VarId(300 + i as u64);
        let t_next = VarId(300 + (i + 1) as u64);

        let v_t_prev = push_node(&mut nodes, CoreFrame::Var(t_prev));
        current_body = push_node(
            &mut nodes,
            CoreFrame::Case {
                scrutinee: v_t_prev,
                binder: VarId(400 + i as u64),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(CONS),
                        binders: vec![VarId(500 + i as u64), t_next],
                        body: current_body,
                    },
                    Alt {
                        con: AltCon::DataAlt(NIL),
                        binders: vec![],
                        body: val_m999,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: val_m999,
                    },
                ],
            },
        );
    }

    let root = wrap_effect(&mut nodes, tag, 0, VarId(300), current_body);
    let _ = root;
    RecursiveTree { nodes }
}

fn build_return_list(tag: u64) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];
    let x = push_node(&mut nodes, CoreFrame::Var(VarId(100)));
    let val_x = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![x],
        },
    );
    let root = wrap_effect(&mut nodes, tag, 0, VarId(100), val_x);
    let _ = root;
    RecursiveTree { nodes }
}

fn build_force_twice(tag: u64) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];

    let b1 = VarId(501);
    let b2 = VarId(502);
    let v_b1 = push_node(&mut nodes, CoreFrame::Var(b1));
    let v_b2 = push_node(&mut nodes, CoreFrame::Var(b2));
    let sum = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![v_b1, v_b2],
        },
    );
    let val_sum = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![sum],
        },
    );

    let v_h = push_node(&mut nodes, CoreFrame::Var(VarId(500)));
    let inner_case2 = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_h,
            binder: b2,
            alts: vec![Alt {
                con: AltCon::Default,
                binders: vec![],
                body: val_sum,
            }],
        },
    );
    let v_h2 = push_node(&mut nodes, CoreFrame::Var(VarId(500)));
    let inner_case1 = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_h2,
            binder: b1,
            alts: vec![Alt {
                con: AltCon::Default,
                binders: vec![],
                body: inner_case2,
            }],
        },
    );

    let v_x = push_node(&mut nodes, CoreFrame::Var(VarId(100)));
    let v_m1 = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(-1)));
    let outer_case = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_x,
            binder: VarId(101),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![VarId(500), VarId(503)],
                    body: inner_case1,
                },
                Alt {
                    con: AltCon::DataAlt(NIL),
                    binders: vec![],
                    body: v_m1,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: v_m1,
                },
            ],
        },
    );

    let root = wrap_effect(&mut nodes, tag, 0, VarId(100), outer_case);
    let _ = root;
    RecursiveTree { nodes }
}

fn build_two_streams() -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];

    let h = VarId(600);
    let h2 = VarId(601);
    let h3 = VarId(602);

    let v_h = push_node(&mut nodes, CoreFrame::Var(h));
    let v_h2 = push_node(&mut nodes, CoreFrame::Var(h2));
    let sum1 = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![v_h, v_h2],
        },
    );
    let v_h3 = push_node(&mut nodes, CoreFrame::Var(h3));
    let sum2 = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![sum1, v_h3],
        },
    );
    let val_sum = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![sum2],
        },
    );

    let v_t2 = push_node(&mut nodes, CoreFrame::Var(VarId(603)));
    let case_t2 = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_t2,
            binder: VarId(604),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![h3, VarId(605)],
                    body: val_sum,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum,
                },
            ],
        },
    );
    let v_ys = push_node(&mut nodes, CoreFrame::Var(VarId(200)));
    let case_ys = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_ys,
            binder: VarId(201),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![h2, VarId(603)],
                    body: case_t2,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum,
                },
            ],
        },
    );

    let body_eff1 = wrap_effect(&mut nodes, 1, 0, VarId(200), case_ys);

    let v_xs = push_node(&mut nodes, CoreFrame::Var(VarId(100)));
    let case_xs = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_xs,
            binder: VarId(101),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![h, VarId(606)],
                    body: body_eff1,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum,
                },
            ],
        },
    );

    let root = wrap_effect(&mut nodes, 0, 0, VarId(100), case_xs);
    let _ = root;
    RecursiveTree { nodes }
}

fn build_b_then_a_tail() -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];

    let h_a = VarId(600);
    let t_a = VarId(601);

    // Sum first 3 of B (ys)
    let b1 = VarId(801);
    let b2 = VarId(802);
    let b3 = VarId(803);
    let v_b1 = push_node(&mut nodes, CoreFrame::Var(b1));
    let v_b2 = push_node(&mut nodes, CoreFrame::Var(b2));
    let v_b3 = push_node(&mut nodes, CoreFrame::Var(b3));
    let s1 = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![v_b1, v_b2],
        },
    );
    let s2 = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![s1, v_b3],
        },
    );
    let val_sum_b = push_node(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![s2],
        },
    );

    // Force A's SECOND chunk using a loop.
    // join walk(n, list) = case n == 0 of True -> val_sum_b; False -> case list of (:) h t -> Jump walk(n-1, t)
    let n_var = VarId(900);
    let list_var = VarId(901);
    let h_loop = VarId(902);
    let t_loop = VarId(903);
    let walk_id = JoinId(100);

    let zero = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(0)));
    let v_n = push_node(&mut nodes, CoreFrame::Var(n_var));
    let n_is_zero = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntEq,
            args: vec![v_n, zero],
        },
    );

    let one = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(1)));
    let n_minus_1 = push_node(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntSub,
            args: vec![v_n, one],
        },
    );
    let v_t_loop = push_node(&mut nodes, CoreFrame::Var(t_loop));
    let jump_walk = push_node(
        &mut nodes,
        CoreFrame::Jump {
            label: walk_id,
            args: vec![n_minus_1, v_t_loop],
        },
    );

    let v_list = push_node(&mut nodes, CoreFrame::Var(list_var));
    let case_list = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_list,
            binder: VarId(904),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![h_loop, t_loop],
                    body: jump_walk,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum_b,
                },
            ],
        },
    );

    // Branch on unboxed IntEq result (1=True, 0=False)
    let walk_body = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: n_is_zero,
            binder: VarId(905),
            alts: vec![
                Alt {
                    con: AltCon::LitAlt(Literal::LitInt(1)),
                    binders: vec![],
                    body: val_sum_b,
                }, // True
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: case_list,
                }, // False
            ],
        },
    );

    // Join binds walk_id
    let count_256 = push_node(&mut nodes, CoreFrame::Lit(Literal::LitInt(256)));
    let v_t_a = push_node(&mut nodes, CoreFrame::Var(t_a));
    let jump_initial = push_node(
        &mut nodes,
        CoreFrame::Jump {
            label: walk_id,
            args: vec![count_256, v_t_a],
        },
    );
    let join_walk = push_node(
        &mut nodes,
        CoreFrame::Join {
            label: walk_id,
            params: vec![n_var, list_var],
            rhs: walk_body,
            body: jump_initial,
        },
    );

    // Unroll sum 3 of ys
    let v_t_b2 = push_node(&mut nodes, CoreFrame::Var(VarId(703)));
    let case_b3 = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_t_b2,
            binder: VarId(704),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![b3, VarId(705)],
                    body: join_walk,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum_b,
                },
            ],
        },
    );
    let v_t_b1 = push_node(&mut nodes, CoreFrame::Var(VarId(701)));
    let case_b2 = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_t_b1,
            binder: VarId(702),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![b2, VarId(703)],
                    body: case_b3,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum_b,
                },
            ],
        },
    );
    let v_ys = push_node(&mut nodes, CoreFrame::Var(VarId(200)));
    let case_b1 = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_ys,
            binder: VarId(201),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![b1, VarId(701)],
                    body: case_b2,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum_b,
                },
            ],
        },
    );

    let body_eff_b = wrap_effect(&mut nodes, 1, 0, VarId(200), case_b1);

    let v_xs = push_node(&mut nodes, CoreFrame::Var(VarId(100)));
    let case_xs = push_node(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: v_xs,
            binder: VarId(101),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![h_a, t_a],
                    body: body_eff_b,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: val_sum_b,
                },
            ],
        },
    );

    let root = wrap_effect(&mut nodes, 0, 0, VarId(100), case_xs);
    let _ = root;
    RecursiveTree { nodes }
}

struct OrderedHandler {
    responses: VecDeque<Response>,
}
impl EffectHandler for OrderedHandler {
    type Request = Value;
    fn handle(&mut self, _req: Value, _cx: &EffectContext) -> Result<Response, EffectError> {
        self.responses
            .pop_front()
            .ok_or(EffectError::Handler("script exhausted".into()))
    }
}

unsafe impl Send for OrderedHandler {}

type Handlers = frunk::HList![OrderedHandler, OrderedHandler];

fn run_prog(
    expr: CoreExpr,
    mut handlers: Handlers,
    nursery_size: usize,
) -> Result<Value, JitError> {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let table = make_table();
            let mut machine = JitEffectMachine::compile(&expr, &table, nursery_size)?;
            machine.run(&table, &mut handlers, &())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn decode_list(mut curr: Value) -> Vec<i64> {
    let mut res = vec![];
    loop {
        let (h, t) = if let Value::Con(ref tag, ref mut fields) = curr {
            if *tag != CONS || fields.len() != 2 {
                break;
            }
            let h = std::mem::replace(&mut fields[0], Value::Lit(Literal::LitInt(0)));
            let t = std::mem::replace(&mut fields[1], Value::Lit(Literal::LitInt(0)));
            (h, t)
        } else {
            break;
        };
        if let Value::Lit(Literal::LitInt(i)) = h {
            res.push(i);
        }
        curr = t;
    }
    res
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

#[test]
fn p2_laziness_quantification() {
    let len = 600;
    let data: Vec<i64> = (0..len as i64).collect();

    {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let src = InstrumentedSource {
            inner: SeqSource {
                data: data.clone(),
                pos: 0,
            },
            stats: stats.clone(),
        };
        let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
        let handlers = frunk::hlist![
            OrderedHandler {
                responses: vec![Response::Stream(stream)].into()
            },
            OrderedHandler {
                responses: vec![].into()
            }
        ];
        let res = run_prog(build_sum_chain(3, 0), handlers, 4 << 20).expect("P2a failed");
        assert!(values_equal(
            &res,
            &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(3))])
        ));
        assert_eq!(
            stats.borrow().next_calls,
            256,
            "Should pull exactly one chunk"
        );
    }

    {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let src = InstrumentedSource {
            inner: IdxSource {
                data: data.clone(),
                pos: 0,
            },
            stats: stats.clone(),
        };
        let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
        let handlers = frunk::hlist![
            OrderedHandler {
                responses: vec![Response::Stream(stream)].into()
            },
            OrderedHandler {
                responses: vec![].into()
            }
        ];
        let res = run_prog(build_sum_chain(3, 0), handlers, 4 << 20).expect("P2b failed");
        assert!(values_equal(
            &res,
            &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(3))])
        ));
        assert_eq!(
            stats.borrow().get_calls,
            3,
            "Should call get() exactly 3 times"
        );
    }

    {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let src = InstrumentedSource {
            inner: IdxSource {
                data: data.clone(),
                pos: 0,
            },
            stats: stats.clone(),
        };
        let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
        let handlers = frunk::hlist![
            OrderedHandler {
                responses: vec![Response::Stream(stream)].into()
            },
            OrderedHandler {
                responses: vec![].into()
            }
        ];
        let res = run_prog(build_spine_walk(280, 0), handlers, 4 << 20).expect("P2c failed");
        assert!(values_equal(
            &res,
            &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(280))])
        ));
        assert_eq!(
            stats.borrow().get_calls,
            0,
            "Should call get() exactly 0 times for spine walk"
        );
    }

    {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let src = InstrumentedSource {
            inner: IdxSource {
                data: data.clone(),
                pos: 0,
            },
            stats: stats.clone(),
        };
        let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
        let handlers = frunk::hlist![
            OrderedHandler {
                responses: vec![Response::Stream(stream)].into()
            },
            OrderedHandler {
                responses: vec![].into()
            }
        ];
        let res = run_prog(build_force_twice(0), handlers, 4 << 20).expect("P2d failed");
        assert!(values_equal(
            &res,
            &Value::Lit(Literal::LitInt(data[0] * 2))
        ));
        assert_eq!(stats.borrow().get_calls, 1, "Should memoize second force");
    }

    {
        let k = 257;
        let stats = Rc::new(RefCell::new(Stats::default()));
        let src = InstrumentedSource {
            inner: InfiniteGuardedSource {
                served: 0,
                guard: k + 600,
            },
            stats: stats.clone(),
        };
        let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
        let handlers = frunk::hlist![
            OrderedHandler {
                responses: vec![Response::Stream(stream)].into()
            },
            OrderedHandler {
                responses: vec![].into()
            }
        ];
        let res = run_prog(build_sum_chain(k, 0), handlers, 4 << 20).expect("P2e failed");
        let expected_sum: i64 = (0..k as i64).sum();
        assert!(values_equal(
            &res,
            &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(expected_sum))])
        ));
        assert!(
            stats.borrow().next_calls <= 512,
            "Pulled too many chunks: {}",
            stats.borrow().next_calls
        );
    }
}

#[test]
fn p3_registry_isolation() {
    let data_a = vec![10, 20, 30];
    let data_b = vec![1, 2, 3];

    let stats_a = Rc::new(RefCell::new(Stats::default()));
    let stats_b = Rc::new(RefCell::new(Stats::default()));

    let src_a = InstrumentedSource {
        inner: SeqSource {
            data: data_a.clone(),
            pos: 0,
        },
        stats: stats_a.clone(),
    };
    let src_b = InstrumentedSource {
        inner: IdxSource {
            data: data_b.clone(),
            pos: 0,
        },
        stats: stats_b.clone(),
    };

    let stream_a = ValueStream::from_source(Box::new(src_a), CONS, NIL);
    let stream_b = ValueStream::from_source(Box::new(src_b), CONS, NIL);

    let handlers = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(stream_a)].into()
        },
        OrderedHandler {
            responses: vec![Response::Stream(stream_b)].into()
        }
    ];

    let res = run_prog(build_two_streams(), handlers, 4 << 20).expect("P3 failed");
    assert!(values_equal(
        &res,
        &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(13))])
    ));

    assert!(stats_a.borrow().dropped);
    assert!(stats_b.borrow().dropped);
}

#[test]
fn p4_abandon_reenter() {
    let data = vec![100, 200, 300, 400];
    let table = make_table();

    let mut machine = JitEffectMachine::compile(&build_sum_chain(2, 0), &table, 4 << 20).unwrap();

    let stats1 = Rc::new(RefCell::new(Stats::default()));
    let src1 = InstrumentedSource {
        inner: IdxSource {
            data: data.clone(),
            pos: 0,
        },
        stats: stats1.clone(),
    };
    let mut handlers1 = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(src1),
                CONS,
                NIL
            ))]
            .into()
        },
        OrderedHandler {
            responses: vec![].into()
        }
    ];

    let res1 = machine
        .run(&table, &mut handlers1, &())
        .expect("Run 1 failed");
    assert!(values_equal(
        &res1,
        &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(300))])
    ));
    assert!(
        stats1.borrow().dropped,
        "Source 1 should be dropped after Run 1"
    );

    let expr2 = build_return_list(0);
    let mut machine2 = JitEffectMachine::compile(&expr2, &table, 4 << 20).unwrap();
    let stats2 = Rc::new(RefCell::new(Stats::default()));
    let src2 = InstrumentedSource {
        inner: SeqSource {
            data: vec![99],
            pos: 0,
        },
        stats: stats2.clone(),
    };
    let mut handlers2 = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(src2),
                CONS,
                NIL
            ))]
            .into()
        },
        OrderedHandler {
            responses: vec![].into()
        }
    ];

    let res2 = machine2
        .run(&table, &mut handlers2, &())
        .expect("Run 2 failed");
    assert_eq!(decode_list(res2), vec![99]);
    assert!(stats2.borrow().dropped);
    assert!(stats1.borrow().drop_order < stats2.borrow().drop_order);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(50))]

    #[test]
    fn p1_model_equivalence(len in prop_oneof![Just(0), Just(1), Just(255), Just(256), Just(257), 2usize..600]) {
        let data: Vec<i64> = (0..len as i64).collect();

        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: SeqSource { data: data.clone(), pos: 0 }, stats: stats.clone() };
            let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(stream)].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            let res = run_prog(build_return_list(0), handlers, 4<<20).expect("P1 SeqSource failed");
            assert_eq!(decode_list(res), data);
            assert!(stats.borrow().dropped, "Source not dropped");
        }

        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: IdxSource { data: data.clone(), pos: 0 }, stats: stats.clone() };
            let stream = ValueStream::from_source(Box::new(src), CONS, NIL);
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(stream)].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            let res = run_prog(build_return_list(0), handlers, 4<<20).expect("P1 IdxSource failed");
            assert_eq!(decode_list(res), data);
            assert!(stats.borrow().dropped, "Source not dropped");
        }
    }

    #[test]
    fn p6_panic_containment(panic_at in prop_oneof![Just(0), Just(1), Just(255), Just(256), Just(257), 2usize..600]) {
        let data: Vec<i64> = (0..600).collect();

        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: PanicSeqSource { data: data.clone(), pos: 0, panic_at }, stats: stats.clone() };
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src), CONS, NIL))].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            let res = run_prog(build_return_list(0), handlers, 4<<20);
            match res {
                Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
                    assert!(msg.contains("panicked"), "Error msg should contain 'panicked', got: {}", msg);
                }
                other => panic!("Expected UserErrorMsg with panic message, got {:?}", other),
            }
            assert!(stats.borrow().dropped);
        }

        {
            let stats_a = Rc::new(RefCell::new(Stats::default()));
            let stats_b = Rc::new(RefCell::new(Stats::default()));
            let src_a = InstrumentedSource { inner: PanicSeqSource { data: (0..600).collect(), pos: 0, panic_at: 300 }, stats: stats_a.clone() };
            let src_b = InstrumentedSource { inner: SeqSource { data: vec![1, 2, 3], pos: 0 }, stats: stats_b.clone() };

            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src_a), CONS, NIL))].into() },
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src_b), CONS, NIL))].into() }
            ];

            let res = run_prog(build_two_streams(), handlers, 4<<20).expect("P6b failed");
            assert!(values_equal(&res, &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(3))])));
            assert!(stats_a.borrow().dropped);
            assert!(stats_b.borrow().dropped);
        }

        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: LyingLenSource { actual: vec![1, 2], claimed: 10, pos: 0 }, stats: stats.clone() };
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src), CONS, NIL))].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            let res = run_prog(build_return_list(0), handlers, 4<<20);
            match res {
                Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
                    assert!(msg.contains("out of bounds"), "Expected OOB error, got: {}", msg);
                }
                other => panic!("Expected UserErrorMsg OOB, got {:?}", other),
            }
        }
    }

    #[test]
    fn p8_stream_element_panic_containment(panic_idx in prop_oneof![Just(0), Just(1), Just(255), Just(256), 0usize..100]) {
        let data: Vec<i64> = (0..600).collect();
        let k = panic_idx + 1;

        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: PanicIdxSource { data: data.clone(), panic_idx, pos: 0 }, stats: stats.clone() };
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src), CONS, NIL))].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            // build_sum_chain(k) with k > panic_idx → run must be Err
            let res = run_prog(build_sum_chain(k, 0), handlers, 4<<20);
            match res {
                Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
                    assert!(msg.contains("panicked"), "Expected 'panicked' in error msg, got: {}", msg);
                }
                other => panic!("Expected UserErrorMsg with panic, got {:?}", other),
            }
            assert!(stats.borrow().dropped);
        }

        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: PanicIdxSource { data: data.clone(), panic_idx, pos: 0 }, stats: stats.clone() };
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src), CONS, NIL))].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            // build_spine_walk(k) with k ≤ panic_idx+1 is tricky if k == panic_idx+1
            // but the instructions say "build_spine_walk(k) with k ≤ len" and "panicking element is never converted"
            // If we walk k elements, we only check CONS cells, we don't force heads.
            let res = run_prog(build_spine_walk(data.len(), 0), handlers, 4<<20).expect("Spine walk should be immune to element panic");
            assert!(values_equal(&res, &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(data.len() as i64))])));
            assert_eq!(stats.borrow().get_calls, 0, "Spine walk should not force heads");
            assert!(stats.borrow().dropped);
        }
    }

    #[test]
    fn p9_bridge_error_conversion_failure(fail_at in prop_oneof![Just(0), Just(1), Just(255), Just(256), 0usize..600]) {
        let data: Vec<i64> = (0..600).collect();

        // Sequential (indexed=false)
        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: FailingConvSource { data: data.clone(), fail_at, indexed: false, pos: 0 }, stats: stats.clone() };
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src), CONS, NIL))].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            let res = run_prog(build_return_list(0), handlers, 4<<20);
            match res {
                Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
                    assert!(msg.contains("conversion failed"), "Expected 'conversion failed', got: {}", msg);
                }
                other => panic!("Expected UserErrorMsg conversion failed, got {:?}", other),
            }
            assert!(stats.borrow().dropped);
        }

        // Indexed (indexed=true)
        {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource { inner: FailingConvSource { data: data.clone(), fail_at, indexed: true, pos: 0 }, stats: stats.clone() };
            let handlers = frunk::hlist![
                OrderedHandler { responses: vec![Response::Stream(ValueStream::from_source(Box::new(src), CONS, NIL))].into() },
                OrderedHandler { responses: vec![].into() }
            ];
            let res = run_prog(build_sum_chain(fail_at + 1, 0), handlers, 4<<20);
            match res {
                Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
                    assert!(msg.contains("conversion failed"), "Expected 'conversion failed', got: {}", msg);
                }
                other => panic!("Expected UserErrorMsg conversion failed, got {:?}", other),
            }
            assert!(stats.borrow().dropped);
        }
    }
}

#[test]
fn fencepost_census() {
    for len in [0, 1, 255, 256, 257] {
        for is_idx in [false, true] {
            let data: Vec<i64> = (0..len as i64).collect();
            let stats = Rc::new(RefCell::new(Stats::default()));
            let stream = if is_idx {
                ValueStream::from_source(
                    Box::new(InstrumentedSource {
                        inner: IdxSource {
                            data: data.clone(),
                            pos: 0,
                        },
                        stats: stats.clone(),
                    }),
                    CONS,
                    NIL,
                )
            } else {
                ValueStream::from_source(
                    Box::new(InstrumentedSource {
                        inner: SeqSource {
                            data: data.clone(),
                            pos: 0,
                        },
                        stats: stats.clone(),
                    }),
                    CONS,
                    NIL,
                )
            };

            let handlers = frunk::hlist![
                OrderedHandler {
                    responses: vec![Response::Stream(stream)].into()
                },
                OrderedHandler {
                    responses: vec![].into()
                }
            ];
            let res = run_prog(build_return_list(0), handlers, 4 << 20).expect("census failed");
            assert_eq!(decode_list(res), data);
            assert!(stats.borrow().dropped);
        }
    }
}

#[test]
fn p7_fork_contained_gc() {
    for len in [256, 400, 600] {
        let mut fds = [0i32; 2];
        unsafe {
            libc::pipe(fds.as_mut_ptr());
        }
        let (rd, wr) = (fds[0], fds[1]);

        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe {
                libc::close(rd);
            }
            let data: Vec<i64> = (0..len as i64).collect();
            let stats = Rc::new(RefCell::new(Stats::default()));
            let src = InstrumentedSource {
                inner: SeqSource {
                    data: data.clone(),
                    pos: 0,
                },
                stats: stats.clone(),
            };
            let handlers = frunk::hlist![
                OrderedHandler {
                    responses: vec![Response::Stream(ValueStream::from_source(
                        Box::new(src),
                        CONS,
                        NIL
                    ))]
                    .into()
                },
                OrderedHandler {
                    responses: vec![].into()
                }
            ];

            let res = run_prog(build_return_list(0), handlers, 512 * 1024);
            let verdict: u8 = match res {
                Ok(v) => {
                    if decode_list(v) == data {
                        1
                    } else {
                        0
                    }
                }
                Err(JitError::Yield(YieldError::HeapOverflow)) => 2,
                _ => 0,
            };
            unsafe {
                libc::write(wr, &verdict as *const u8 as *const libc::c_void, 1);
                libc::_exit(0);
            }
        }

        unsafe {
            libc::close(wr);
        }
        let mut res_byte = [0u8; 1];
        let n = unsafe { libc::read(rd, res_byte.as_mut_ptr() as *mut libc::c_void, 1) };
        unsafe {
            libc::close(rd);
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            assert!(n > 0, "Child died without verdict (B3 bug if GC failed)");
            assert!(
                res_byte[0] == 1 || res_byte[0] == 2,
                "GC verdict failed: {}",
                res_byte[0]
            );
        }
    }
}

#[test]
fn t_panic_payload_nonstring() {
    struct PanicAnySource;
    impl ValueSource for PanicAnySource {
        fn next_value(
            &mut self,
            _table: &DataConTable,
        ) -> Option<Result<Value, tidepool_bridge::BridgeError>> {
            std::panic::panic_any(42i32);
        }
    }

    let handlers = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(PanicAnySource),
                CONS,
                NIL
            ))]
            .into()
        },
        OrderedHandler {
            responses: vec![].into()
        }
    ];
    let res = run_prog(build_return_list(0), handlers, 4 << 20);
    match res {
        Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
            assert!(
                msg.contains("<non-string panic>"),
                "Expected fallback msg, got: {}",
                msg
            );
        }
        _ => panic!("Expected UserErrorMsg"),
    }
}

#[test]
fn t_seq_len256_pull_count() {
    let data: Vec<i64> = (0..256).collect();
    let stats = Rc::new(RefCell::new(Stats::default()));
    let src = InstrumentedSource {
        inner: SeqSource {
            data: data.clone(),
            pos: 0,
        },
        stats: stats.clone(),
    };
    let handlers = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(src),
                CONS,
                NIL
            ))]
            .into()
        },
        OrderedHandler {
            responses: vec![].into()
        }
    ];
    let _ = run_prog(build_return_list(0), handlers, 4 << 20).expect("failed");
    assert_eq!(stats.borrow().next_calls, 257);
}

#[test]
fn t_g3_fired_panic_isolation() {
    let stats_a = Rc::new(RefCell::new(Stats::default()));
    let stats_b = Rc::new(RefCell::new(Stats::default()));
    let src_a = InstrumentedSource {
        inner: PanicSeqSource {
            data: (0..600).collect(),
            pos: 0,
            panic_at: 300,
        },
        stats: stats_a.clone(),
    };
    let src_b = InstrumentedSource {
        inner: SeqSource {
            data: vec![1, 2, 3],
            pos: 0,
        },
        stats: stats_b.clone(),
    };

    let handlers = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(src_a),
                CONS,
                NIL
            ))]
            .into()
        },
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(src_b),
                CONS,
                NIL
            ))]
            .into()
        }
    ];

    let res = run_prog(build_b_then_a_tail(), handlers, 4 << 20);
    match res {
        Err(JitError::Yield(YieldError::UserErrorMsg(msg))) => {
            assert!(msg.contains("panicked"), "Expected panic msg, got: {}", msg);
        }
        other => panic!("Expected UserErrorMsg with panic, got {:?}", other),
    }

    // B was fully drained: 3 Some + 1 None = 4 calls
    assert_eq!(
        stats_b.borrow().next_calls,
        4,
        "B should be fully drained before A panics"
    );
    assert!(stats_a.borrow().dropped);
    assert!(stats_b.borrow().dropped);
}

#[test]
fn t_g4_complete_spine_repark_fenceposts() {
    for len in [1999, 2000, 2001, 2256, 2257, 2500] {
        let data: Vec<i64> = (0..len as i64).collect();
        // Build spine back-to-front
        let mut spine = Value::Con(NIL, vec![]);
        for &v in data.iter().rev() {
            spine = Value::Con(CONS, vec![Value::Lit(Literal::LitInt(v)), spine]);
        }

        // build_return_list(0) will take Response::Complete(spine) from handler 0
        let handlers = frunk::hlist![
            OrderedHandler {
                responses: vec![Response::Complete(spine)].into()
            },
            OrderedHandler {
                responses: vec![].into()
            }
        ];
        let res = run_prog(build_return_list(0), handlers, 4 << 20).expect("G4 return_list failed");
        assert_eq!(decode_list(res), data, "Mismatch at len {}", len);

        // One P_TAKE_SUM(3) at len=2500
        if len == 2500 {
            let mut spine = Value::Con(NIL, vec![]);
            for &v in data.iter().rev() {
                spine = Value::Con(CONS, vec![Value::Lit(Literal::LitInt(v)), spine]);
            }
            let handlers2 = frunk::hlist![
                OrderedHandler {
                    responses: vec![Response::Complete(spine)].into()
                },
                OrderedHandler {
                    responses: vec![].into()
                }
            ];
            let res2 =
                run_prog(build_sum_chain(3, 0), handlers2, 4 << 20).expect("G4 sum_chain failed");
            assert!(values_equal(
                &res2,
                &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(3))])
            ));
        }
    }
}

#[test]
fn t_g5_lying_len_spine_walk_immune() {
    let stats = Rc::new(RefCell::new(Stats::default()));
    let src = InstrumentedSource {
        inner: LyingLenSource {
            actual: vec![1, 2],
            claimed: 10,
            pos: 0,
        },
        stats: stats.clone(),
    };
    let handlers = frunk::hlist![
        OrderedHandler {
            responses: vec![Response::Stream(ValueStream::from_source(
                Box::new(src),
                CONS,
                NIL
            ))]
            .into()
        },
        OrderedHandler {
            responses: vec![].into()
        }
    ];
    // build_spine_walk(10) → Ok(Val(10)) and get_calls == 0
    let res = run_prog(build_spine_walk(10, 0), handlers, 4 << 20).expect("G5 failed");
    assert!(values_equal(
        &res,
        &Value::Con(VAL, vec![Value::Lit(Literal::LitInt(10))])
    ));
    assert_eq!(
        stats.borrow().get_calls,
        0,
        "Spine walk should not call get()"
    );
}
