//! Differential proptest for the JIT effect-dispatch loop.
//!
//! ## What is under test
//!
//! Two independent machines interpret the *same* freer-simple `Eff` program
//! driven by the *same* deterministic response script: the **JIT machine**
//! (`JitEffectMachine::run`, `src/jit_machine.rs`) and the **eval machine**
//! (`tidepool_effect::machine::EffectMachine`, the tree-walking interpreter
//! used here as a differential oracle).
//!
//! Both route effects through the same real nominal `DispatchEffect` HList
//! implementation. This test deliberately varies the freer-simple union tag:
//! it remains an implementation detail of the Haskell library and has no
//! authority over Rust handler selection.
//!
//! ## Crash isolation
//!
//! A JIT (or eval) fault that escapes `with_signal_protection` lands in the
//! process-wide SIGSEGV/SIGILL handler, which terminates the process. Every case
//! runs in a forked child that streams a verdict back over a pipe; the parent
//! attributes faults by verdict-byte presence, including failures that exit
//! without a signal. The child runs the JIT phase first
//! and writes a survival marker before touching the eval oracle, so a missing
//! marker is unambiguously a JIT fault (B3), while a marker with no final record
//! is an eval-side fault (a known-divergence skip, not a bug).
//!
//! ## Reported bug classes
//!
//!  * **B1** — both machines succeed but final values differ.
//!  * **B2** — JIT errors where eval succeeds, outside the known whitelist.
//!  * **B3** — any fatal signal / uncaught fault (verdict absent), including on
//!    shape-mismatched responses.
//!  * **B4** — JIT run-twice nondeterminism.
//!  * **B-transcript** — dispatch order diverges between the two machines even
//!    when final values agree.
//!
//! Known-divergence filters (NOT bugs): eval-side errors/faults on synthetic
//! programs; `HeapOverflow` from a tiny nursery.

#![allow(clippy::needless_range_loop)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use frunk::hlist;
use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};

use tidepool_codegen::host_fns::RuntimeError;
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
        // I# boxes `respond_list` integer elements (i64::ToCore).
        (8, "I#", 1, 1),
    ] {
        t.insert(DataCon {
            id: DataConId(id),
            name: name.into(),
            tag,
            rep_arity: ar,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
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
    /// `Stream(0..n)` via `respond_list` — an eager list response. Sizes are
    /// chosen around chunk boundaries (255/256/257/4096).
    Stream(usize),
    /// `Complete(Lit(String))` fed into an integer continuation — shape
    /// mismatch; a clean error is required, never a fatal trap.
    Str(String),
    /// `Complete(Lit(Double bits))` fed into an integer continuation. The
    /// double's raw bits are NOT a valid `Int#`; eval's strict `expect_int`
    /// rejects it, so the JIT must reject it too rather than bit-reinterpret
    /// the float payload as a "number" (proptest_jit_dispatch B2, FP residual:
    /// the original guard only rejected pointer-valued lits).
    Double(f64),
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
            Spec::Stream(n) => cx.respond_list((0..*n as i64).collect::<Vec<i64>>()),
            Spec::Str(s) => Ok(Value::Lit(Literal::LitString(s.clone().into_bytes())).into()),
            Spec::Double(d) => Ok(Value::Lit(Literal::LitDouble(d.to_bits())).into()),
            Spec::Err => Err(EffectError::Handler("scripted error".into())),
        }
    }
}

/// Shared per-machine state: a global dispatch cursor into the script plus a
/// transcript of requests in dispatch order. One instance per machine — never
/// shared across the JIT and eval runs.
struct Recorder {
    cursor: usize,
    transcript: Vec<i64>,
}

/// A scripted, transcript-recording handler for the test's single nominal
/// request family.
struct ScriptedHandler {
    script: Rc<Vec<Spec>>,
    rec: Rc<RefCell<Recorder>>,
}

impl EffectHandler for ScriptedHandler {
    // `Value` intentionally owns every request in this one-handler test. The
    // effect crate separately tests routing between distinct nominal families.
    type Request = Value;
    fn handle(&mut self, req: Value, cx: &EffectContext) -> Result<Response, EffectError> {
        let pos = {
            let mut r = self.rec.borrow_mut();
            let p = r.cursor;
            r.cursor += 1;
            let Value::Lit(Literal::LitInt(request)) = req else {
                return Err(EffectError::UnexpectedValue {
                    context: "LitInt request",
                    got: format!("{req:?}"),
                });
            };
            r.transcript.push(request);
            p
        };
        self.script
            .get(pos)
            .cloned()
            .unwrap_or(Spec::Int(0))
            .to_response(cx)
    }
}

type Handlers = frunk::HList![ScriptedHandler];

fn make_handlers(script: &Rc<Vec<Spec>>, rec: &Rc<RefCell<Recorder>>) -> Handlers {
    let handler = ScriptedHandler {
        script: script.clone(),
        rec: rec.clone(),
    };
    hlist![handler]
}

fn fresh_rec() -> Rc<RefCell<Recorder>> {
    Rc::new(RefCell::new(Recorder {
        cursor: 0,
        transcript: vec![],
    }))
}

// ---------------------------------------------------------------------------
// Program builders — hand-built effect trees: E(Union(tag, req), Leaf(\x -> ...)).
// ---------------------------------------------------------------------------

/// A single effect's carrier tag and integer request payload. The carrier tag
/// is varied to prove it has no routing meaning.
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
const REC_LEN: usize = 24;

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
    determ: bool,
    eval_ok: bool,
    eval_kind: u8,
    eval_val: i64,
    eval_errclass: u8,
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
        b[11] = self.determ as u8;
        b[12] = self.eval_ok as u8;
        b[13] = self.eval_kind;
        b[14..22].copy_from_slice(&self.eval_val.to_le_bytes());
        b[22] = self.eval_errclass;
        // 23: packed booleans
        b[23] = (self.values_match as u8) | ((self.transcript_match as u8) << 1);
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
            determ: b[11] != 0,
            eval_ok: b[12] != 0,
            eval_kind: b[13],
            eval_val: i64at(14),
            eval_errclass: b[22],
            values_match: b[23] & 1 != 0,
            transcript_match: b[23] & 2 != 0,
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

/// Total: every `EffectError` variant is named, no wildcard arm — a variant
/// added upstream breaks this match at compile time rather than silently
/// falling into a catch-all.
fn eval_err_class(e: &EffectError) -> (u8, i64) {
    match e {
        EffectError::UnhandledEffect { .. } => (errclass::UNHANDLED, -1),
        EffectError::Eval(_) => (errclass::EVAL, -1),
        EffectError::Bridge(_) => (errclass::BRIDGE, -1),
        EffectError::Handler(_) => (errclass::HANDLER, -1),
        EffectError::MissingConstructor { .. } => (errclass::OTHER, -1),
        EffectError::FieldCountMismatch { .. } => (errclass::OTHER, -1),
        EffectError::UnexpectedValue { .. } => (errclass::OTHER, -1),
    }
}

/// Total: every `RuntimeError` variant is named, no wildcard arm.
fn runtime_err_class(e: &RuntimeError) -> (u8, i64) {
    match e {
        RuntimeError::CaseTrap => (errclass::CASE_TRAP, -1),
        RuntimeError::HeapOverflow => (errclass::HEAP_OVERFLOW, -1),
        RuntimeError::DivisionByZero
        | RuntimeError::Overflow
        | RuntimeError::UserError
        | RuntimeError::UserErrorMsg(_)
        | RuntimeError::PatternMatchFailure(_)
        | RuntimeError::Undefined
        | RuntimeError::BadPointer
        | RuntimeError::TypeMetadata
        | RuntimeError::UnresolvedExternal(_)
        | RuntimeError::UnresolvedVar(..)
        | RuntimeError::NullFunPtr
        | RuntimeError::BadFunPtrTag(_)
        | RuntimeError::StackOverflow
        | RuntimeError::BlackHole
        | RuntimeError::BadThunkState(_)
        | RuntimeError::Cancelled => (errclass::OTHER, -1),
    }
}

/// Total: every `YieldError` variant is named.
fn yield_err_class(e: &YieldError) -> (u8, i64) {
    match e {
        YieldError::Signal(_) => (errclass::SIGNAL, -1),
        YieldError::Runtime(rt) => runtime_err_class(rt),
        YieldError::UnexpectedTag(_) => (errclass::OTHER, -1),
        YieldError::UnexpectedConTag(_) => (errclass::OTHER, -1),
        YieldError::BadValFields(_) => (errclass::OTHER, -1),
        YieldError::BadEFields(_) => (errclass::OTHER, -1),
        YieldError::BadUnionFields(_) => (errclass::OTHER, -1),
        YieldError::NullPointer => (errclass::OTHER, -1),
    }
}

/// Total: every `JitError` variant is named, no wildcard arm. Doesn't route
/// through a pre-collapsed effect-error class: this lane needs the
fn jit_err_class(e: &JitError) -> (u8, i64) {
    match e {
        JitError::Effect(eff) => eval_err_class(eff),
        JitError::HeapBridge(_) => (errclass::BRIDGE, -1),
        JitError::Signal(_) => (errclass::SIGNAL, -1),
        JitError::EffectResponseTooLarge { .. } => (errclass::TOO_LARGE, -1),
        JitError::Yield(y) => yield_err_class(y),
        JitError::Compilation(_) => (errclass::OTHER, -1),
        JitError::Pipeline(_) => (errclass::OTHER, -1),
        JitError::MissingConTags(_) => (errclass::OTHER, -1),
        JitError::InvalidSuspensionState(_)
        | JitError::UnknownContinuation(_)
        | JitError::UnknownValueHandle(_)
        | JitError::EmptyProjection => (errclass::OTHER, -1),
        JitError::VarIdCollision(_) => (errclass::OTHER, -1),
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
) -> (Result<Value, JitError>, Vec<i64>) {
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
) -> (Result<Value, EffectError>, Vec<i64>) {
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

    let (jit_ok, jit_kind, jit_val, jit_errclass) = match &jit1 {
        Ok(v) => {
            let (k, n) = val_summary(v);
            (true, k, n, errclass::NONE)
        }
        Err(e) => {
            let (c, t) = jit_err_class(e);
            let _ = t;
            (false, 0, 0, c)
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
    let (eval_ok, eval_kind, eval_val, eval_errclass) = match &eval {
        Ok(v) => {
            let (k, n) = val_summary(v);
            (true, k, n, errclass::NONE)
        }
        Err(e) => {
            let (c, t) = eval_err_class(e);
            let _ = t;
            (false, 0, 0, c)
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
        determ,
        eval_ok,
        eval_kind,
        eval_val,
        eval_errclass,
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
    } else if data.len() > REC_LEN {
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

/// Assert the full differential contract for a program expected to run to a
/// final value on the eval oracle. Returns `Ok(true)` if
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

fn carrier_tag() -> impl Strategy<Value = u64> {
    0u64..256
}

/// An arithmetic chain (1..6 effects, all integer responses) whose irrelevant
/// carrier tags vary independently of the requests.
fn arith_chain_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    prop::collection::vec((carrier_tag(), -1000i64..1000i64), 1..=6).prop_map(|pairs| {
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

/// A single effect returning a huge `Complete` list or a `Stream` at
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
    (carrier_tag(), sizes)
        .prop_map(|(tag, spec)| (build_list_head(&Eff { tag, req: 0 }), vec![spec]))
}

/// An arithmetic chain where the handler at a chosen position errors.
fn err_at_k_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    (prop::collection::vec(carrier_tag(), 1..=6))
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

/// A single effect whose integer continuation receives a wrong-shape
/// response — a string (pointer-valued lit) or a double (float-class lit). Both
/// are rejected by eval's strict `expect_int`, so the JIT must reject them too
/// rather than reinterpret a pointer / IEEE-754 payload as `Int#`.
fn shape_mismatch_strategy() -> impl Strategy<Value = (CoreExpr, Vec<Spec>)> {
    let wrong_shape = prop_oneof![
        "[a-z]{0,8}".prop_map(Spec::Str),
        (-1e9f64..1e9f64).prop_map(Spec::Double),
    ];
    (carrier_tag(), wrong_shape)
        .prop_map(|(tag, spec)| (build_arith1(&Eff { tag, req: 0 }), vec![spec]))
}

// ---------------------------------------------------------------------------
// Properties.
//
// `full_differential` and `huge_complete_and_stream` drive `TestRunner`
// directly (not the `proptest!` macro) so each can own a local, in-process
// reach floor (`ReachTally`) for its bespoke `Outcome`/`Verdict` type.
// `err_at_k` / `shape_mismatch_resume` stay on the `proptest!` macro: they
// probe error paths where "reached a value comparison" is not the metric.
//
// A hand-built `Config` driven through `TestRunner` must set `source_file`
// explicitly: `FileFailurePersistence::SourceParallel` (proptest's default)
// resolves the `.proptest-regressions` path purely off `source_file` and
// silently resolves to NO PATH without it — the `proptest!` macro sets this
// implicitly from `file!()`, a bare `TestRunner` does not.
// ---------------------------------------------------------------------------

/// A local, in-process reach counter for this lane's bespoke `Outcome` type.
/// `Cell`, not an atomic: `TestRunner::run` drives the case closure through
/// `Fn`, not `FnMut`, but each property runs single-threaded.
struct ReachTally {
    label: &'static str,
    total: Cell<u64>,
    reached: Cell<u64>,
}

impl ReachTally {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            total: Cell::new(0),
            reached: Cell::new(0),
        }
    }

    fn record(&self, reached: bool) {
        self.total.set(self.total.get() + 1);
        if reached {
            self.reached.set(self.reached.get() + 1);
        }
    }

    fn assert_floor(&self, min_ratio: f64) {
        let (total, reached) = (self.total.get(), self.reached.get());
        let pct = if total > 0 {
            100.0 * reached as f64 / total as f64
        } else {
            0.0
        };
        eprintln!("{} REACH: {}/{} ({:.1}%)", self.label, reached, total, pct);
        assert!(
            total > 0,
            "{}: reach floor asserted over zero cases — an empty run is not a pass",
            self.label
        );
        let ratio = reached as f64 / total as f64;
        assert!(
            ratio >= min_ratio,
            "{}: reach floor failed — {}/{} ({:.1}%) is below the required {:.1}%",
            self.label,
            reached,
            total,
            pct,
            min_ratio * 100.0
        );
    }
}

/// Full differential: arithmetic chains with arbitrary carrier tags. Both
/// machines must agree
/// on the final value AND the dispatch sequence; the JIT must be
/// deterministic across two runs.
#[test]
fn full_differential() {
    let mut cfg = PtConfig::with_cases(200);
    cfg.source_file = Some(file!()); // TestRunner-driven, not the proptest! macro — see cfg note above.
    let reach = ReachTally::new("jit-dispatch/full_differential");
    let mut runner = TestRunner::new(cfg);
    runner
        .run(&arith_chain_strategy(), |(expr, script)| {
            let outcome = run_case(expr, script);
            let reached = assert_differential(&outcome)?;
            reach.record(reached);
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.80);
}

/// Huge `Complete` lists and chunk-boundary `Stream`s: the JIT's spine
/// dismantle / re-park / parked-iterator paths must reduce to the same head
/// element the eval oracle computes, with no fatal fault.
#[test]
fn huge_complete_and_stream() {
    let mut cfg = PtConfig::with_cases(100);
    cfg.source_file = Some(file!());
    let reach = ReachTally::new("jit-dispatch/huge_complete_and_stream");
    let mut runner = TestRunner::new(cfg);
    runner
        .run(&huge_strategy(), |(expr, script)| {
            let outcome = run_case(expr, script);
            let reached = assert_differential(&outcome)?;
            reach.record(reached);
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.80);
}

proptest! {
    #![proptest_config(PtConfig::with_cases(100))]

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

    /// Shape-mismatched response (string into an integer continuation).
    ///
    /// CONTRACT: a clean error — never a fatal trap, never a silently-wrong
    /// value. The unboxing loops guard the Con-unwrap step (boxing wrappers
    /// have exactly one field; see `emit_boxing_wrapper_guard`), so a
    /// multi-field Con where a number was expected traps cleanly instead of
    /// yielding pointer-derived garbage.
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
// Captured bugs — minimal, deterministic repros, both fixed and now active
// regression tests. Seeds for the generated forms live in
// proptest_jit_dispatch.proptest-regressions.
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
// FIXED (emit_boxing_wrapper_guard in emit/primop.rs): the unbox loops trap
// cleanly on a multi-field Con, so the string response yields a clean error
// exactly like eval. Active regression test.
#[test]
fn bug_shape_mismatch_jit_reads_string_as_int() {
    // Minimal shrunk form: E(Union(0, 0), Leaf(\x -> Val(x +# 7))) with the
    // handler answering Complete(Lit("")).
    let expr = build_arith1(&Eff { tag: 0, req: 0 });
    let script = vec![Spec::Str(String::new())];

    match run_case(expr, script) {
        Outcome::Rec(v) => {
            assert!(
                !v.eval_ok,
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

/// BUG (B2 / silent-garbage, FP residual): a handler returns a `Double` where
/// the continuation does `Int#` arithmetic. The original guard only
/// rejected *pointer*-valued lits (STRING / arrays), so a `Double` lit slipped
/// through `unbox_int` and its raw IEEE-754 bits were loaded as an `i64` — a
/// silently-wrong number where the eval oracle's strict `expect_int` cleanly
/// errors.
///
///  * observed (pre-fix): JIT `run` returns `Ok(Lit(LitInt(<double bits as
///    i64>)))`; eval returns `Err(EffectError::Eval(TypeMismatch))`.
///  * expected: the JIT surfaces a clean error, never a bit-reinterpreted
///    number, when an `Int#` continuation forces a floating-point response.
///  * class: B2 (JIT-only divergence; eval errors, JIT "succeeds" with garbage).
///  * component: compiled `IntAdd` primop's numeric unbox
///    (`tidepool-codegen/src/emit/primop.rs` `unbox_numeric` lit-tag guard).
///  * note: only ill-typed Core reaches this (well-typed GHC emits an explicit
///    `Double2Int`/`Int2Double`), so it is a defensive-robustness gap. Unlike
///    Word#/Char# — which `unbox_int` *legitimately* accepts for `Ord#`/Word
///    ops — a float-class lit is never a valid `Int#` source, so rejecting it
///    cannot regress valid programs.
// FIXED (class-compatible lit-tag guard in unbox_numeric): an `I64` numeric
// unbox now accepts only INT/WORD/CHAR lits and traps cleanly on a
// FLOAT/DOUBLE (or pointer) lit. Active regression test.
#[test]
fn bug_shape_mismatch_jit_reads_double_as_int() {
    // E(Union(0, 0), Leaf(\x -> Val(x +# 7))) with the handler answering
    // Complete(Lit(Double 3.5)).
    let expr = build_arith1(&Eff { tag: 0, req: 0 });
    let script = vec![Spec::Double(3.5)];

    match run_case(expr, script) {
        Outcome::Rec(v) => {
            assert!(
                !v.eval_ok,
                "oracle precondition: eval must reject double-into-Int#"
            );
            assert!(
                !v.jit_ok,
                "B2 regressed: eval rejected double+#Int but JIT returned Ok(int kind={}, val={}) — \
                 the resume path read the double's IEEE-754 bits as Int#",
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

/// Enumerate a fixed, RNG-free spread of arithmetic chains and prove:
///  * at least 80% reach final-value comparison (both machines produce a value);
///  * the transcript (dispatch sequence) is actually compared on every reached
///    case — the counter is non-trivial and every comparison agreed.
///
/// This is the explicit evidence that sequences (not just final values) are
/// compared, and that coverage clears the bar.
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
                    // Include values far beyond the former handler-list width:
                    // carrier tags must not select Rust handlers.
                    tag: (seed << (i % 3)) % 256,
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
                    "coverage audit: JIT fault on an arithmetic chain (len={} seed={})",
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
        "only {}/{} ({:.0}%) of cases reached final-value comparison; need >=80%",
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
