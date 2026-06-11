//! Differential proptest for the JIT effect-dispatch loop (W5 jit-dispatch).
//!
//! ## What is under test
//!
//! Two independent machines interpret the *same* freer-simple `Eff` program
//! driven by the *same* deterministic response script:
//!
//!  * the **JIT machine** — `JitEffectMachine::run`, whose dispatch loop lives
//!    in `tidepool-codegen/src/jit_machine.rs:259-430` (request out via
//!    `heap_to_value_forcing`, response in via `value_to_heap` / stream
//!    parking / huge-spine dismantle);
//!  * the **eval machine** — `tidepool_effect::machine::EffectMachine`, the
//!    tree-walking interpreter used here as a differential oracle.
//!
//! Both peel effect tags through the *same* real `DispatchEffect` HList impl
//! (`tidepool-effect/src/dispatch.rs:266-280`, tag 0 → head, N → tail with
//! N-1). Because routing is shared code, a routing off-by-one cannot surface
//! as a JIT/eval divergence — so tag routing is probed directly instead
//! (invalid-tag cases: clean `UnhandledEffect` with the same decremented tag
//! on both sides, and never a fatal signal).
//!
//! ## Why a real frunk HList (the one Cargo.toml deviation)
//!
//! The only `DispatchEffect` impls in the tree are for frunk's `HCons`/`HNil`,
//! and there is no reachable re-export. Driving both machines through a
//! hand-rolled dispatcher would test a *mirror* of the routing logic rather
//! than the real thing. So `frunk` is added as a **dev-dependency only**
//! (test-scoped; the production crate graph is untouched). This is the minimal
//! change that makes the specified oracle faithful.
//!
//! ## Crash isolation
//!
//! A JIT (or eval) fault that escapes `with_signal_protection` lands in the
//! process-wide SIGSEGV/SIGILL handler, which `SYS_exit`s the *thread* — to the
//! embedder that reads as a silent hang, not a failure (see
//! `signal_safety.rs:316-330` and `.tidepool/crash.log`). So every case runs in
//! a forked child that streams a verdict back over a pipe; the parent attributes
//! faults by *verdict-byte presence*, not by `WIFSIGNALED` (the handler would
//! mask the signal as a clean thread exit). The child runs the JIT phase first
//! and writes a survival marker before touching the eval oracle, so a missing
//! marker is unambiguously a JIT fault (B3), while a marker with no final record
//! is an eval-side fault (a known-divergence skip, not a bug).
//!
//! ## Reported bug classes
//!
//!  * **B1** — both machines succeed but final values differ.
//!  * **B2** — JIT errors where eval succeeds, outside the known whitelist.
//!  * **B3** — any fatal signal / uncaught fault (verdict absent), including on
//!    invalid tags and shape-mismatched responses.
//!  * **B4** — JIT run-twice nondeterminism.
//!  * **B-transcript** — dispatch sequence (handler indices, in order) diverges
//!    between the two machines even when final values agree.
//!
//! Known-divergence filters (NOT bugs): eval-side errors/faults on synthetic
//! programs; `HeapOverflow` from a tiny nursery.

#![allow(clippy::needless_range_loop)]

use std::cell::RefCell;
use std::rc::Rc;

use frunk::hlist;
use proptest::prelude::*;
use proptest::test_runner::Config as PtConfig;

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_effect::dispatch::{EffectContext, EffectHandler, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::machine::EffectMachine;
use tidepool_eval::heap::VecHeap;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree};
use tidepool_testing::proptest::values_equal;

// Fixed handler arity: tags 0..N_HANDLERS are valid, N_HANDLERS..256 invalid.
const N_HANDLERS: u64 = 4;
const NURSERY: usize = 1 << 20;
const CHILD_STACK: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// DataConTable with the freer-simple constructors + list constructors.
// ---------------------------------------------------------------------------

fn make_table() -> DataConTable {
    let mut t = DataConTable::new();
    for (id, name, tag, ar) in [
        (1u64, "Val", 1u32, 1u32),
        (2, "E", 2, 2),
        (3, "Leaf", 1, 1),
        (4, "Node", 2, 2),
        (5, "Union", 1, 2),
        (6, ":", 2, 2),
        (7, "[]", 1, 0),
        // I# boxes `respond_stream`/`respond_list` integer elements (i64::ToCore).
        (8, "I#", 1, 1),
    ] {
        t.insert(DataCon {
            id: DataConId(id),
            name: name.into(),
            tag,
            rep_arity: ar,
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    t
}

const VAL: DataConId = DataConId(1);
const E: DataConId = DataConId(2);
const LEAF: DataConId = DataConId(3);
const UNION: DataConId = DataConId(5);
const CONS: DataConId = DataConId(6);
const NIL: DataConId = DataConId(7);

// ---------------------------------------------------------------------------
// Response script: one entry per dispatch, in dispatch order.
// ---------------------------------------------------------------------------

/// A scripted handler response. Chosen to exercise every materialization path
/// in the JIT dispatch loop.
#[derive(Clone, Debug)]
enum Spec {
    /// `Complete(Lit(n))` — the classic small value path.
    Int(i64),
    /// `Complete(<cons spine of length n>)` — probes `probe_list_spine` /
    /// `dismantle_list_spine` / re-park (n past `LAZY_SPINE_THRESHOLD_NODES`).
    HugeList(usize),
    /// `Stream(0..n)` via `respond_stream` — parked iterator source. Sizes are
    /// chosen around chunk boundaries (255/256/257/4096).
    Stream(usize),
    /// `Complete(Lit(String))` fed into an integer continuation — shape
    /// mismatch; a clean error is required, never a fatal trap.
    Str(String),
    /// Handler returns `Err` at this dispatch position (trampoline error path).
    Err,
}

impl Spec {
    fn to_response(&self, cx: &EffectContext) -> Result<Response, EffectError> {
        match self {
            Spec::Int(n) => Ok(Value::Lit(Literal::LitInt(*n)).into()),
            Spec::HugeList(n) => {
                // Built iteratively (a recursive builder would overflow before
                // the machine ever sees the value).
                let mut acc = Value::Con(NIL, vec![]);
                for i in (0..*n).rev() {
                    acc = Value::Con(CONS, vec![Value::Lit(Literal::LitInt(i as i64)), acc]);
                }
                Ok(acc.into())
            }
            Spec::Stream(n) => cx.respond_stream(0..*n as i64),
            Spec::Str(s) => Ok(Value::Lit(Literal::LitString(s.clone().into_bytes())).into()),
            Spec::Err => Err(EffectError::Handler("scripted error".into())),
        }
    }
}

/// Shared per-machine state: a global dispatch cursor into the script plus a
/// transcript of (dispatch_index, handler_tag) in dispatch order. One instance
/// per machine — never shared across the JIT and eval runs.
struct Recorder {
    cursor: usize,
    transcript: Vec<(usize, u64)>,
}

/// A scripted, transcript-recording handler. Every HList slot holds one,
/// carrying its slot index (= the tag it answers when reached at tag 0).
struct ScriptedHandler {
    index: u64,
    script: Rc<Vec<Spec>>,
    rec: Rc<RefCell<Recorder>>,
}

impl EffectHandler for ScriptedHandler {
    // `Value` so the raw request reaches us unchanged (its FromCore is identity)
    // — shape mismatches must not be intercepted at the dispatch boundary.
    type Request = Value;
    fn handle(&mut self, _req: Value, cx: &EffectContext) -> Result<Response, EffectError> {
        let pos = {
            let mut r = self.rec.borrow_mut();
            let p = r.cursor;
            r.cursor += 1;
            r.transcript.push((p, self.index));
            p
        };
        self.script
            .get(pos)
            .cloned()
            .unwrap_or(Spec::Int(0))
            .to_response(cx)
    }
}

type Handlers = frunk::HList![
    ScriptedHandler,
    ScriptedHandler,
    ScriptedHandler,
    ScriptedHandler
];

fn make_handlers(script: &Rc<Vec<Spec>>, rec: &Rc<RefCell<Recorder>>) -> Handlers {
    let mk = |index| ScriptedHandler {
        index,
        script: script.clone(),
        rec: rec.clone(),
    };
    hlist![mk(0), mk(1), mk(2), mk(3)]
}

fn fresh_rec() -> Rc<RefCell<Recorder>> {
    Rc::new(RefCell::new(Recorder {
        cursor: 0,
        transcript: vec![],
    }))
}

// ---------------------------------------------------------------------------
// Program builders — hand-built effect trees generalizing
// proptest_effect_machine.rs's E(Union(tag, req), Leaf(\x -> ...)) constructors.
// ---------------------------------------------------------------------------

/// A single effect's coordinates: which handler tag fires and the integer
/// request payload.
#[derive(Clone, Debug)]
struct Eff {
    tag: u64,
    req: i64,
}

/// Chain of effects whose continuations thread each response into a running
/// integer sum: `E(Union(t0,r0), Leaf(\x0 -> E(Union(t1,r1), Leaf(\x1 ->
/// ... Val(x0 +# x1 +# ...)))))`. Final value = sum of all responses. Valid
/// only when every response is integer-typed (Sum forces each `xi`).
fn build_sum_chain(effs: &[Eff]) -> CoreExpr {
    let n = effs.len();
    let binder = |i: usize| VarId(100 + i as u64);
    let mut nodes: Vec<CoreFrame<usize>> = vec![];
    let push = |nodes: &mut Vec<CoreFrame<usize>>, f: CoreFrame<usize>| {
        nodes.push(f);
        nodes.len() - 1
    };

    // Final accumulator: Val(x0 +# x1 +# ... +# x_{n-1}).
    let mut acc = push(&mut nodes, CoreFrame::Var(binder(0)));
    for i in 1..n {
        let vi = push(&mut nodes, CoreFrame::Var(binder(i)));
        acc = push(
            &mut nodes,
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![acc, vi],
            },
        );
    }
    let mut rest = push(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![acc],
        },
    );

    // Wrap from the innermost effect outward.
    for i in (0..n).rev() {
        rest = wrap_effect(&mut nodes, effs[i].tag, effs[i].req, binder(i), rest);
    }
    RecursiveTree { nodes }
}

/// Single effect whose continuation reduces a *list* response to a scalar:
/// `E(Union(t,req), Leaf(\x -> case x of { (:) h _ -> Val(h); [] -> Val(-1) }))`.
/// Final value = the first element, or -1 for an empty list. Keeps the final
/// value a scalar so a huge response never recurses through `values_equal`.
fn build_list_head(eff: &Eff) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];
    let push = |nodes: &mut Vec<CoreFrame<usize>>, f: CoreFrame<usize>| {
        nodes.push(f);
        nodes.len() - 1
    };
    let x = push(&mut nodes, CoreFrame::Var(VarId(100)));
    let h = push(&mut nodes, CoreFrame::Var(VarId(900)));
    let val_h = push(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![h],
        },
    );
    let m1 = push(&mut nodes, CoreFrame::Lit(Literal::LitInt(-1)));
    let val_m1 = push(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![m1],
        },
    );
    let case = push(
        &mut nodes,
        CoreFrame::Case {
            scrutinee: x,
            binder: VarId(901),
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CONS),
                    binders: vec![VarId(900), VarId(902)],
                    body: val_h,
                },
                Alt {
                    con: AltCon::DataAlt(NIL),
                    binders: vec![],
                    body: val_m1,
                },
            ],
        },
    );
    let root = wrap_effect(&mut nodes, eff.tag, eff.req, VarId(100), case);
    let _ = root;
    RecursiveTree { nodes }
}

/// Single effect whose continuation does integer arithmetic on the response:
/// `E(Union(t,req), Leaf(\x -> Val(x +# 7)))`. With an integer response this
/// is well-typed; with a string response it is the shape-mismatch probe.
fn build_arith1(eff: &Eff) -> CoreExpr {
    let mut nodes: Vec<CoreFrame<usize>> = vec![];
    let push = |nodes: &mut Vec<CoreFrame<usize>>, f: CoreFrame<usize>| {
        nodes.push(f);
        nodes.len() - 1
    };
    let x = push(&mut nodes, CoreFrame::Var(VarId(100)));
    let k = push(&mut nodes, CoreFrame::Lit(Literal::LitInt(7)));
    let sum = push(
        &mut nodes,
        CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![x, k],
        },
    );
    let val = push(
        &mut nodes,
        CoreFrame::Con {
            tag: VAL,
            fields: vec![sum],
        },
    );
    let _ = wrap_effect(&mut nodes, eff.tag, eff.req, VarId(100), val);
    RecursiveTree { nodes }
}

/// Append `E(Union(tag, req), Leaf(\binder -> <body>))` and return its index.
/// `body` must already be present in `nodes`.
fn wrap_effect(
    nodes: &mut Vec<CoreFrame<usize>>,
    tag: u64,
    req: i64,
    binder: VarId,
    body: usize,
) -> usize {
    let lam = {
        nodes.push(CoreFrame::Lam { binder, body });
        nodes.len() - 1
    };
    let leaf = {
        nodes.push(CoreFrame::Con {
            tag: LEAF,
            fields: vec![lam],
        });
        nodes.len() - 1
    };
    let req_n = {
        nodes.push(CoreFrame::Lit(Literal::LitInt(req)));
        nodes.len() - 1
    };
    let tag_n = {
        nodes.push(CoreFrame::Lit(Literal::LitWord(tag)));
        nodes.len() - 1
    };
    let union = {
        nodes.push(CoreFrame::Con {
            tag: UNION,
            fields: vec![tag_n, req_n],
        });
        nodes.len() - 1
    };
    nodes.push(CoreFrame::Con {
        tag: E,
        fields: vec![union, leaf],
    });
    nodes.len() - 1
}

// ---------------------------------------------------------------------------
// Verdict record streamed child -> parent over the pipe.
// ---------------------------------------------------------------------------

/// Survival marker written after the JIT phase, before the eval oracle runs.
const MARKER: u8 = 0xA1;
/// Fixed-size verdict payload following the marker.
const REC_LEN: usize = 40;

/// Error class buckets (stable across `EffectError`/`JitError`).
mod errclass {
    pub const NONE: u8 = 0;
    pub const UNHANDLED: u8 = 1;
    pub const EVAL: u8 = 2;
    pub const BRIDGE: u8 = 3;
    pub const SIGNAL: u8 = 4;
    pub const CASE_TRAP: u8 = 5;
    pub const HEAP_OVERFLOW: u8 = 6;
    pub const OTHER: u8 = 7;
    pub const HANDLER: u8 = 8;
    pub const TOO_LARGE: u8 = 9;
}

#[derive(Clone, Debug)]
struct Verdict {
    jit_ok: bool,
    jit_kind: u8,
    jit_val: i64,
    jit_errclass: u8,
    jit_unhandled_tag: i64,
    determ: bool,
    eval_ok: bool,
    eval_kind: u8,
    eval_val: i64,
    eval_errclass: u8,
    eval_unhandled_tag: i64,
    values_match: bool,
    transcript_match: bool,
}

impl Verdict {
    fn to_bytes(&self) -> [u8; REC_LEN] {
        let mut b = [0u8; REC_LEN];
        b[0] = self.jit_ok as u8;
        b[1] = self.jit_kind;
        b[2..10].copy_from_slice(&self.jit_val.to_le_bytes());
        b[10] = self.jit_errclass;
        b[11..19].copy_from_slice(&self.jit_unhandled_tag.to_le_bytes());
        b[19] = self.determ as u8;
        b[20] = self.eval_ok as u8;
        b[21] = self.eval_kind;
        b[22..30].copy_from_slice(&self.eval_val.to_le_bytes());
        b[30] = self.eval_errclass;
        b[31..39].copy_from_slice(&self.eval_unhandled_tag.to_le_bytes());
        // 39: packed booleans
        b[39] = (self.values_match as u8) | ((self.transcript_match as u8) << 1);
        b
    }

    fn from_bytes(b: &[u8]) -> Verdict {
        let i64at = |o: usize| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[o..o + 8]);
            i64::from_le_bytes(a)
        };
        Verdict {
            jit_ok: b[0] != 0,
            jit_kind: b[1],
            jit_val: i64at(2),
            jit_errclass: b[10],
            jit_unhandled_tag: i64at(11),
            determ: b[19] != 0,
            eval_ok: b[20] != 0,
            eval_kind: b[21],
            eval_val: i64at(22),
            eval_errclass: b[30],
            eval_unhandled_tag: i64at(31),
            values_match: b[39] & 1 != 0,
            transcript_match: b[39] & 2 != 0,
        }
    }
}

/// Parent-side outcome of a forked case.
#[derive(Clone, Debug)]
enum Outcome {
    /// JIT faulted uncaught (no survival marker) — B3.
    JitFault,
    /// JIT survived but the eval oracle faulted — known-divergence skip.
    EvalFault,
    /// Both phases completed; verdict available.
    Rec(Verdict),
}

fn val_summary(v: &Value) -> (u8, i64) {
    match v {
        Value::Lit(Literal::LitInt(n)) => (1, *n),
        Value::Lit(Literal::LitWord(w)) => (2, *w as i64),
        Value::Lit(Literal::LitString(_)) => (3, 0),
        Value::Con(id, _) => (4, id.0 as i64),
        _ => (0, 0),
    }
}

fn eval_err_class(e: &EffectError) -> (u8, i64) {
    match e {
        EffectError::UnhandledEffect { tag } => (errclass::UNHANDLED, *tag as i64),
        EffectError::Eval(_) => (errclass::EVAL, -1),
        EffectError::Bridge(_) => (errclass::BRIDGE, -1),
        EffectError::Handler(_) => (errclass::HANDLER, -1),
        _ => (errclass::OTHER, -1),
    }
}

fn jit_err_class(e: &JitError) -> (u8, i64) {
    match e {
        JitError::Effect(EffectError::UnhandledEffect { tag }) => {
            (errclass::UNHANDLED, *tag as i64)
        }
        JitError::Effect(EffectError::Eval(_)) => (errclass::EVAL, -1),
        JitError::Effect(EffectError::Bridge(_)) => (errclass::BRIDGE, -1),
        JitError::Effect(EffectError::Handler(_)) => (errclass::HANDLER, -1),
        JitError::Effect(_) => (errclass::OTHER, -1),
        JitError::HeapBridge(_) => (errclass::BRIDGE, -1),
        JitError::Signal(_) => (errclass::SIGNAL, -1),
        JitError::EffectResponseTooLarge { .. } => (errclass::TOO_LARGE, -1),
        JitError::Yield(y) => match y {
            YieldError::Signal(_) => (errclass::SIGNAL, -1),
            YieldError::CaseTrap => (errclass::CASE_TRAP, -1),
            YieldError::HeapOverflow => (errclass::HEAP_OVERFLOW, -1),
            _ => (errclass::OTHER, -1),
        },
        _ => (errclass::OTHER, -1),
    }
}

// ---------------------------------------------------------------------------
// Differential runner — executed inside the forked child.
// ---------------------------------------------------------------------------

/// Run the JIT machine once against a fresh handler set; returns the result
/// plus the recorded transcript.
fn run_jit(
    expr: &CoreExpr,
    table: &DataConTable,
    script: &Rc<Vec<Spec>>,
) -> (Result<Value, JitError>, Vec<(usize, u64)>) {
    let rec = fresh_rec();
    let mut handlers = make_handlers(script, &rec);
    let res = match JitEffectMachine::compile(expr, table, NURSERY) {
        Ok(mut m) => m.run(table, &mut handlers, &()),
        Err(e) => Err(e),
    };
    let t = rec.borrow().transcript.clone();
    (res, t)
}

fn run_eval(
    expr: &CoreExpr,
    table: &DataConTable,
    script: &Rc<Vec<Spec>>,
) -> (Result<Value, EffectError>, Vec<(usize, u64)>) {
    let rec = fresh_rec();
    let mut handlers = make_handlers(script, &rec);
    let mut heap = VecHeap::new();
    let res = match EffectMachine::new(table, &mut heap) {
        Ok(mut m) => m.run_with_user(expr, &mut handlers, &()),
        Err(e) => Err(e),
    };
    let t = rec.borrow().transcript.clone();
    (res, t)
}

/// Child body: JIT-first (with survival marker), then the eval oracle, then a
/// verdict record. Writes to `fd`. Never returns — `_exit`s the child.
fn child_run(expr: &CoreExpr, script: Vec<Spec>, fd: i32) -> ! {
    // 20s watchdog: a genuine hang (uncaught) leaves the parent reading EOF.
    unsafe {
        libc::alarm(20);
    }
    let table = make_table();
    let script = Rc::new(script);

    // --- JIT phase (twice, for determinism) ---
    let (jit1, jlog1) = run_jit(expr, &table, &script);
    let (jit2, _jlog2) = run_jit(expr, &table, &script);

    // Survived the JIT — emit the marker so the parent can tell a JIT fault
    // (marker absent) from an eval fault (marker present, record absent).
    write_all(fd, &[MARKER]);

    let (jit_ok, jit_kind, jit_val, jit_errclass, jit_unhandled_tag) = match &jit1 {
        Ok(v) => {
            let (k, n) = val_summary(v);
            (true, k, n, errclass::NONE, -1)
        }
        Err(e) => {
            let (c, t) = jit_err_class(e);
            (false, 0, 0, c, t)
        }
    };
    let determ = match (&jit1, &jit2) {
        (Ok(a), Ok(b)) => values_equal(a, b),
        (Err(_), Err(_)) => {
            let (ca, _) = jit_err_class(jit1.as_ref().err().unwrap());
            let (cb, _) = jit_err_class(jit2.as_ref().err().unwrap());
            ca == cb
        }
        _ => false,
    };

    // --- eval oracle phase ---
    let (eval, elog) = run_eval(expr, &table, &script);
    let (eval_ok, eval_kind, eval_val, eval_errclass, eval_unhandled_tag) = match &eval {
        Ok(v) => {
            let (k, n) = val_summary(v);
            (true, k, n, errclass::NONE, -1)
        }
        Err(e) => {
            let (c, t) = eval_err_class(e);
            (false, 0, 0, c, t)
        }
    };

    let values_match = match (&eval, &jit1) {
        (Ok(a), Ok(b)) => values_equal(a, b),
        _ => false,
    };
    let transcript_match = elog == jlog1;

    let verdict = Verdict {
        jit_ok,
        jit_kind,
        jit_val,
        jit_errclass,
        jit_unhandled_tag,
        determ,
        eval_ok,
        eval_kind,
        eval_val,
        eval_errclass,
        eval_unhandled_tag,
        values_match,
        transcript_match,
    };
    write_all(fd, &verdict.to_bytes());
    unsafe {
        libc::close(fd);
        libc::_exit(0);
    }
}

fn write_all(fd: i32, buf: &[u8]) {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe {
            libc::write(
                fd,
                buf[off..].as_ptr() as *const libc::c_void,
                buf.len() - off,
            )
        };
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}

/// Fork a child that runs the differential and stream a verdict back. The
/// child inherits the caller's (8 MiB) stack. Attribution is by byte presence:
/// no marker → JIT fault; marker but no record → eval fault.
fn fork_case(expr: &CoreExpr, script: Vec<Spec>) -> Outcome {
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 with a 2-int array is the documented contract.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed");
    let (rd, wr) = (fds[0], fds[1]);

    // SAFETY: fork in a single-threaded-from-here child; child only touches
    // async-safe libc + its own freshly compiled JIT state.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            libc::close(rd);
        }
        child_run(expr, script, wr);
    }
    // Parent.
    unsafe {
        libc::close(wr);
    }
    let mut data = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(rd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    unsafe {
        libc::close(rd);
        let mut status = 0i32;
        libc::waitpid(pid, &mut status, 0);
    }

    if data.is_empty() || data[0] != MARKER {
        Outcome::JitFault
    } else if data.len() >= 1 + REC_LEN {
        Outcome::Rec(Verdict::from_bytes(&data[1..1 + REC_LEN]))
    } else {
        Outcome::EvalFault
    }
}

/// Run a case on an 8 MiB stack so the forked child inherits enough stack for
/// deep eval-side spines.
fn run_case(expr: CoreExpr, script: Vec<Spec>) -> Outcome {
    std::thread::Builder::new()
        .stack_size(CHILD_STACK)
        .spawn(move || fork_case(&expr, script))
        .unwrap()
        .join()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Shared assertion helpers.
// ---------------------------------------------------------------------------

/// Assert the full differential contract for a case whose valid-tag program is
/// expected to run to a final value on the eval oracle. Returns `Ok(true)` if
/// the case reached final-value comparison (both machines produced a value).
fn assert_differential(outcome: &Outcome) -> Result<bool, TestCaseError> {
    match outcome {
        Outcome::JitFault => {
            prop_assert!(
                false,
                "B3: JIT faulted (fatal signal / uncaught) — no verdict produced"
            );
            unreachable!()
        }
        Outcome::EvalFault => {
            // Eval oracle faulted on a synthetic program: known divergence.
            Ok(false)
        }
        Outcome::Rec(v) => {
            if v.eval_ok && v.jit_ok {
                prop_assert!(
                    v.values_match,
                    "B1: final values differ — eval=({},{}) jit=({},{})",
                    v.eval_kind,
                    v.eval_val,
                    v.jit_kind,
                    v.jit_val
                );
                prop_assert!(
                    v.transcript_match,
                    "B-transcript: dispatch sequences diverge between JIT and eval"
                );
                prop_assert!(v.determ, "B4: JIT run-twice nondeterminism");
                Ok(true)
            } else if v.eval_ok && !v.jit_ok {
                // JIT errored where eval succeeded — whitelist HeapOverflow only.
                prop_assert!(
                    v.jit_errclass == errclass::HEAP_OVERFLOW,
                    "B2: JIT failed (errclass={}) but eval succeeded ({}, {})",
                    v.jit_errclass,
                    v.eval_kind,
                    v.eval_val
                );
                Ok(false)
            } else {
                // eval failed (or both): known-divergence skip.
                Ok(false)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Strategies.
// ---------------------------------------------------------------------------

fn valid_tag() -> impl Strategy<Value = u64> {
    0u64..N_HANDLERS
}

fn invalid_tag() -> impl Strategy<Value = u64> {
    // Includes 255 explicitly (cf. nested_mapm_tag255 off-by-one history).
    prop_oneof![
        N_HANDLERS..256u64,
        Just(255u64),
        Just(N_HANDLERS),
        Just(N_HANDLERS + 1)
    ]
}

/// A valid-tag arithmetic chain (1..6 effects, all integer responses).
fn arith_chain_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    prop::collection::vec((valid_tag(), -1000i64..1000i64), 1..=6).prop_map(|pairs| {
        let effs: Vec<Eff> = pairs.iter().map(|&(tag, req)| Eff { tag, req }).collect();
        let script: Vec<Spec> = pairs.iter().map(|&(_, _)| Spec::Int(0)).collect();
        // Make responses distinct so the sum is sensitive to ordering.
        let script: Vec<Spec> = script
            .into_iter()
            .enumerate()
            .map(|(i, _)| Spec::Int((i as i64 + 1) * 7))
            .collect();
        (build_sum_chain(&effs), script)
    })
}

/// A single valid-tag effect returning a huge `Complete` list or a `Stream` at
/// chunk-boundary sizes; continuation reduces to the head element.
fn huge_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    let sizes = prop_oneof![
        (2000usize..4200usize).prop_map(Spec::HugeList),
        prop_oneof![
            Just(255usize),
            Just(256),
            Just(257),
            Just(4096),
            (1usize..300usize),
        ]
        .prop_map(Spec::Stream),
    ];
    (valid_tag(), sizes).prop_map(|(tag, spec)| (build_list_head(&Eff { tag, req: 0 }), vec![spec]))
}

/// A valid-tag arithmetic chain where the handler at a chosen position errors.
fn err_at_k_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    (prop::collection::vec(valid_tag(), 1..=6))
        .prop_flat_map(|tags| {
            let n = tags.len();
            (Just(tags), 0usize..n)
        })
        .prop_map(|(tags, k)| {
            let effs: Vec<Eff> = tags.iter().map(|&tag| Eff { tag, req: 0 }).collect();
            let mut script: Vec<Spec> = (0..tags.len()).map(|i| Spec::Int(i as i64)).collect();
            script[k] = Spec::Err;
            (build_sum_chain(&effs), script)
        })
}

/// A chain of valid arithmetic effects terminated by one invalid-tag effect.
fn invalid_tag_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    (
        prop::collection::vec(valid_tag(), 0..=4),
        invalid_tag(),
        -1000i64..1000i64,
    )
        .prop_map(|(valids, bad, req)| {
            let mut effs: Vec<Eff> = valids.iter().map(|&tag| Eff { tag, req: 0 }).collect();
            effs.push(Eff { tag: bad, req });
            let script: Vec<Spec> = (0..effs.len()).map(|i| Spec::Int(i as i64)).collect();
            (build_sum_chain(&effs), script)
        })
}

/// A single valid-tag effect whose integer continuation receives a string —
/// the shape-mismatch probe.
fn shape_mismatch_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    (valid_tag(), "[a-z]{0,8}")
        .prop_map(|(tag, s)| (build_arith1(&Eff { tag, req: 0 }), vec![Spec::Str(s)]))
}

// ---------------------------------------------------------------------------
// Properties.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(PtConfig::with_cases(200))]

    /// Full differential: valid-tag arithmetic chains. Both machines must agree
    /// on the final value AND the dispatch sequence; the JIT must be
    /// deterministic across two runs.
    #[test]
    fn full_differential((expr, script) in arith_chain_strategy()) {
        let outcome = run_case(expr, script);
        assert_differential(&outcome)?;
    }
}

proptest! {
    #![proptest_config(PtConfig::with_cases(100))]

    /// Huge `Complete` lists and chunk-boundary `Stream`s: the JIT's spine
    /// dismantle / re-park / parked-iterator paths must reduce to the same head
    /// element the eval oracle computes, with no fatal fault.
    #[test]
    fn huge_complete_and_stream((expr, script) in huge_strategy()) {
        let outcome = run_case(expr, script);
        assert_differential(&outcome)?;
    }

    /// Handler errors mid-chain (trampoline error path): both machines must
    /// stop at the same dispatch and neither may fault.
    #[test]
    fn err_at_k((expr, script) in err_at_k_strategy()) {
        let outcome = run_case(expr, script);
        match outcome {
            Outcome::JitFault => prop_assert!(false, "B3: JIT faulted on handler error path"),
            Outcome::EvalFault => {}
            Outcome::Rec(v) => {
                // Handler error → both machines error (not a value).
                prop_assert!(!v.jit_ok, "JIT produced a value despite a scripted handler error");
                prop_assert!(!v.eval_ok, "eval produced a value despite a scripted handler error");
                prop_assert!(
                    v.transcript_match,
                    "B-transcript: dispatch sequences diverge on the handler-error path"
                );
            }
        }
    }

    /// Invalid tags must never produce a fatal signal, and must surface a clean
    /// `UnhandledEffect` with the SAME decremented tag (dispatch index) on both
    /// machines. Value comparison is undefined here and is not asserted.
    #[test]
    fn invalid_tag_never_signals((expr, script) in invalid_tag_strategy()) {
        let outcome = run_case(expr, script);
        match outcome {
            Outcome::JitFault => {
                prop_assert!(false, "B3: invalid tag produced a fatal signal / uncaught fault");
            }
            Outcome::EvalFault => {}
            Outcome::Rec(v) => {
                prop_assert!(!v.jit_ok, "invalid tag must error, JIT returned a value");
                prop_assert!(!v.eval_ok, "invalid tag must error, eval returned a value");
                prop_assert_eq!(
                    v.jit_errclass, errclass::UNHANDLED,
                    "JIT invalid-tag error was not UnhandledEffect (class {})", v.jit_errclass
                );
                prop_assert_eq!(
                    v.eval_errclass, errclass::UNHANDLED,
                    "eval invalid-tag error was not UnhandledEffect (class {})", v.eval_errclass
                );
                prop_assert_eq!(
                    v.jit_unhandled_tag, v.eval_unhandled_tag,
                    "tag-routing divergence: JIT errored at index {} but eval at {}",
                    v.jit_unhandled_tag, v.eval_unhandled_tag
                );
                prop_assert!(
                    v.transcript_match,
                    "B-transcript: dispatch sequence before the invalid tag diverged"
                );
            }
        }
    }

    /// Shape-mismatched response (string into an integer continuation).
    ///
    /// CONTRACT: a clean error — never a fatal trap, never a silently-wrong
    /// value. FIXED 2026-06-10: the unboxing loops now guard the Con-unwrap
    /// step (boxing wrappers have exactly one field; see
    /// emit_boxing_wrapper_guard), so a multi-field Con where a number was
    /// expected traps cleanly instead of yielding pointer-derived garbage.
    /// The full strict contract is asserted live.
    #[test]
    fn shape_mismatch_resume((expr, script) in shape_mismatch_strategy()) {
        let outcome = run_case(expr, script);
        match outcome {
            Outcome::JitFault => {
                prop_assert!(false, "B3: shape-mismatched response caused a fatal trap (not a clean error)");
            }
            Outcome::Rec(v) => {
                // If eval rejected the shape, the JIT must reject it too —
                // never "succeed" with a garbage number.
                prop_assert!(
                    v.eval_ok || !v.jit_ok,
                    "B2 regressed: eval rejected the shape-mismatched response but \
                     JIT returned Ok(kind={}, val={})", v.jit_kind, v.jit_val
                );
            }
            Outcome::EvalFault => {} // known-divergence skip
        }
    }
}

// ---------------------------------------------------------------------------
// Captured bugs — minimal, deterministic repros. `#[ignore]`d so the suite is
// green; run with `--ignored` to observe the divergence. Seeds for the
// generated forms live in proptest_jit_dispatch.proptest-regressions.
// ---------------------------------------------------------------------------

/// BUG (B2 / silent-garbage): the JIT resume path performs `Int#` arithmetic on
/// a string response by reading the `Text`/string heap object's pointer word as
/// a raw integer, returning a garbage value, where the eval oracle cleanly
/// rejects the type mismatch.
///
///  * observed: JIT `run` returns `Ok(Lit(LitInt(<pointer-derived garbage>)))`
///    (nondeterministic — it is a heap address + 7); eval returns
///    `Err(EffectError::Eval(..))`.
///  * expected: the JIT surfaces a clean error (or a recoverable trap), never a
///    silently-wrong value, when a continuation forces a response of the wrong
///    runtime shape (string where `Int#` is expected).
///  * class: B2 (JIT-only divergence; eval errors, JIT "succeeds" with garbage).
///  * component: JIT effect-dispatch resume → `value_to_heap` of a string
///    response + the compiled `IntAdd` primop's unchecked unbox
///    (`tidepool-codegen/src/jit_machine.rs` resume path +
///    primop integer unboxing).
///  * trigger requires ill-typed Core (a handler whose response type disagrees
///    with the continuation) — well-typed GHC output cannot reach it, so this
///    is a defensive-robustness gap, not a miscompile of valid programs.
///  * seed: proptest cc ee1877d8…84337f0 (shrinks to `Str("")`).
// FIXED 2026-06-10 (emit_boxing_wrapper_guard in emit/primop.rs): the unbox
// loops trap cleanly on a multi-field Con, so the string response yields a
// clean error exactly like eval. Active regression test — the assertion below
// is the desired contract and now passes.
#[test]
fn bug_shape_mismatch_jit_reads_string_as_int() {
    // Minimal shrunk form: E(Union(0, 0), Leaf(\x -> Val(x +# 7))) with the
    // tag-0 handler answering Complete(Lit("")).
    let expr = build_arith1(&Eff { tag: 0, req: 0 });
    let script = vec![Spec::Str(String::new())];

    match run_case(expr, script) {
        Outcome::Rec(v) => {
            assert!(
                v.eval_ok == false,
                "oracle precondition: eval must reject string-into-Int#"
            );
            // FIXED: the JIT must reject it just like eval (clean error, not
            // pointer-derived garbage).
            assert!(
                !v.jit_ok,
                "B2 regressed: eval rejected string+#Int but JIT returned Ok(int kind={}, val={}) — \
                 the resume path read the string heap pointer as Int#",
                v.jit_kind, v.jit_val
            );
        }
        Outcome::JitFault => panic!("expected a (buggy) value, not a fatal fault"),
        Outcome::EvalFault => panic!("eval oracle faulted unexpectedly"),
    }
}

// ---------------------------------------------------------------------------
// Deterministic coverage + transcript audit (the >=80% / counter requirement).
// ---------------------------------------------------------------------------

/// Enumerate a fixed, RNG-free spread of valid-tag arithmetic chains and prove:
///  * at least 80% reach final-value comparison (both machines produce a value);
///  * the transcript (dispatch sequence) is actually compared on every reached
///    case — the counter is non-trivial and every comparison agreed.
///
/// This is the explicit evidence that sequences (not just final values) are
/// compared, and that valid-tag coverage clears the bar.
#[test]
fn coverage_and_transcript_audit() {
    let mut total = 0usize;
    let mut reached = 0usize;
    let mut transcript_compared = 0usize;
    let mut transcript_agreed = 0usize;

    // 6 chain lengths x ~34 tag/req patterns = 200+ deterministic cases.
    for len in 1usize..=6 {
        for seed in 0u64..34 {
            total += 1;
            let effs: Vec<Eff> = (0..len)
                .map(|i| Eff {
                    tag: (seed >> (i % 6)) % N_HANDLERS,
                    req: ((seed as i64 + i as i64) % 11) - 5,
                })
                .collect();
            let script: Vec<Spec> = (0..len).map(|i| Spec::Int((i as i64 + 1) * 13)).collect();
            let expr = build_sum_chain(&effs);
            match run_case(expr, script) {
                Outcome::Rec(v) => {
                    if v.eval_ok && v.jit_ok {
                        reached += 1;
                        transcript_compared += 1;
                        if v.transcript_match {
                            transcript_agreed += 1;
                        }
                        assert!(
                            v.values_match,
                            "coverage audit: value mismatch at len={} seed={}",
                            len, seed
                        );
                    }
                }
                Outcome::JitFault => panic!(
                    "coverage audit: JIT fault on a valid arithmetic chain (len={} seed={})",
                    len, seed
                ),
                Outcome::EvalFault => {}
            }
        }
    }

    // Counter proof: transcripts were actually compared, and all agreed.
    assert!(
        transcript_compared > 0,
        "no transcript comparisons performed — the differential is not exercising dispatch sequences"
    );
    assert_eq!(
        transcript_compared, transcript_agreed,
        "transcript divergence in coverage audit: {}/{} agreed",
        transcript_agreed, transcript_compared
    );
    let ratio = reached as f64 / total as f64;
    assert!(
        ratio >= 0.80,
        "only {}/{} ({:.0}%) of valid-tag cases reached final-value comparison; need >=80%",
        reached,
        total,
        ratio * 100.0
    );
    eprintln!(
        "[coverage] {}/{} ({:.0}%) reached final comparison; {} transcript comparisons, all agreed",
        reached,
        total,
        ratio * 100.0,
        transcript_compared
    );
}
