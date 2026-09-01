//! The classified JIT-vs-eval differential runner.
//!
//! One configurable runner for every differential lane in the workspace. A lane
//! declares a [`DiffConfig`] (which nursery sizes, how many repeats, whether to
//! probe for fatal signals, which error classes are acceptable); the runner
//! executes each engine ONCE per distinct runtime configuration and returns a
//! fully classified [`CaseRun`]. Assertions read the recorded run — they never
//! re-execute.
//!
//! # Contract
//!
//! - **Execute once, assert many.** A `CaseRun` holds the eval result, every
//!   JIT result (per nursery × per repeat), and every crash-probe status. Value
//!   agreement, nursery invariance, determinism, and crash containment are all
//!   read off that one record. Distinct nursery sizes are distinct runtime
//!   configurations and ARE preserved; a second execution of the same
//!   configuration to feed a second assertion is not.
//! - **Total classification.** Every `(eval, jit)` pair lands in exactly one
//!   [`Verdict`]. There is no wildcard arm and no silent discard: an outcome is
//!   either compared, explicitly accepted by the lane's
//!   [`ExpectedErrorPolicy`], or a failure.
//! - **An eval error is a failure by default.** [`ExpectedErrorPolicy::eval`]
//!   is empty unless a lane names classes it tolerates. A lane whose generator
//!   is total and ground therefore goes red the moment the oracle stops
//!   working — which is what makes the differential a gate rather than a smoke
//!   test.
//! - **Every lane states a reach floor.** [`ReachCounter::assert_floor`] is the
//!   `minimum_comparison_reach` gate: a lane that stops reaching value
//!   comparison fails even when nothing diverges.
//!
//! # Shape of a lane
//!
//! The reach floor is asserted in the SAME process as the property that feeds
//! it. nextest runs every test in its own process, so a trailing `zzz_`-ordered
//! floor test observes a zero counter and can never fail — the floor has to
//! close over the property's own run. That means driving `TestRunner` directly
//! instead of using the `proptest!` macro:
//!
//! ```ignore
//! fn dcfg() -> DiffConfig {
//!     DiffConfig::new("case-dispatch")
//!         .nurseries(&[64 * 1024, 4 * 1024])
//!         .repeat_count(2)                     // 2 => determinism is asserted
//!         .crash_containment(CrashContainment::ForkProbe)
//!         .expect_jit(&[JitErrorClass::HeapOverflow])
//! }
//!
//! #[test]
//! fn prop_dense_int() {
//!     let reach = ReachCounter::new("case-dispatch/dense_int");
//!     let mut runner = TestRunner::new(proptest_cfg());
//!     runner
//!         .run(&arb_dense_int(), |spec| {
//!             check(build_dense_int(&spec), &dcfg(), &reach)
//!         })
//!         .unwrap();
//!     reach.assert_floor(0.90);
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::TestCaseError;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::error::EvalError;
use tidepool_eval::value::Value;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::CoreExpr;

use crate::compare::values_equal;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How a lane runs one case: the runtime configurations to sweep and the
/// outcomes it is willing to accept.
#[derive(Debug, Clone)]
pub struct DiffConfig {
    /// Lane name, used in failure messages and reach reports.
    pub label: &'static str,
    /// Nursery sizes to sweep. Each is a distinct runtime configuration: the
    /// JIT is compiled and run once per size (times `repeat_count`). Results
    /// across sizes must agree — the nursery is a runtime knob, and a correct
    /// GC is nursery-invariant.
    pub nurseries: &'static [usize],
    /// JIT executions per nursery size. `1` disables the determinism check;
    /// `2` (or more) asserts every repeat at a given size agrees.
    pub repeat_count: usize,
    /// Whether to probe for fatal signals in a forked child before running the
    /// in-process JIT.
    pub crash_containment: CrashContainment,
    /// The named outcomes this lane tolerates. Anything not named is a failure.
    pub expected: ExpectedErrorPolicy,
    /// Deep-force the eval result to normal form before comparing. The JIT's
    /// result conversion forces lazy fields, so the eval side must observe the
    /// same demand or a bottom hidden under a lazy field reads as a false
    /// divergence. Lanes replaying real captured Core against the extractor's
    /// own `DataConTable` may want WHNF instead.
    pub deep_force_eval: bool,
    /// Arm the hang watchdog and label it with this case's expression. Lanes
    /// that label the watchdog themselves (per fixture name) set this false.
    pub watchdog: bool,
}

/// Whether to run each nursery configuration in a forked child first, to catch
/// a fatal signal (SIGSEGV/SIGILL/SIGBUS) that would otherwise kill the test
/// process outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashContainment {
    /// No fork probe. The lane accepts that a JIT fault kills the process.
    Off,
    /// Fork a child per nursery size; a non-zero termination signal is a
    /// [`Verdict::Crash`].
    ForkProbe,
}

/// The outcomes a lane names as acceptable. Empty means "nothing is acceptable
/// except a successful comparison" — the correct default for a total, ground
/// generator.
#[derive(Debug, Clone, Default)]
pub struct ExpectedErrorPolicy {
    /// JIT failure classes tolerated when eval succeeded. Tolerated at SOME
    /// nursery sizes only: if no nursery size produced a JIT value, the case is
    /// [`Verdict::AcceptedJitError`] (counted, not compared).
    pub jit: &'static [JitErrorClass],
    /// Eval failure classes tolerated. Empty by default: an eval error from a
    /// lane that generates total, ground programs means the oracle is broken.
    pub eval: &'static [EvalErrorClass],
}

impl DiffConfig {
    /// A lane with one 64 KiB nursery, no repeats, no crash probe, and nothing
    /// tolerated but a clean comparison.
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            nurseries: &[64 * 1024],
            repeat_count: 1,
            crash_containment: CrashContainment::Off,
            expected: ExpectedErrorPolicy::default(),
            deep_force_eval: true,
            watchdog: true,
        }
    }

    /// Sweep these nursery sizes (each a distinct runtime configuration).
    pub fn nurseries(mut self, ns: &'static [usize]) -> Self {
        assert!(
            !ns.is_empty(),
            "{}: nurseries must be non-empty",
            self.label
        );
        self.nurseries = ns;
        self
    }

    /// Run the JIT this many times per nursery size. `>= 2` asserts determinism.
    pub fn repeat_count(mut self, n: usize) -> Self {
        assert!(n >= 1, "{}: repeat_count must be >= 1", self.label);
        self.repeat_count = n;
        self
    }

    /// Probe each nursery configuration for fatal signals in a forked child.
    pub fn crash_containment(mut self, c: CrashContainment) -> Self {
        self.crash_containment = c;
        self
    }

    /// Name the JIT failure classes this lane tolerates.
    pub fn expect_jit(mut self, classes: &'static [JitErrorClass]) -> Self {
        self.expected.jit = classes;
        self
    }

    /// Name the eval failure classes this lane tolerates. Every entry weakens
    /// the oracle — justify each one at the call site.
    pub fn expect_eval(mut self, classes: &'static [EvalErrorClass]) -> Self {
        self.expected.eval = classes;
        self
    }

    /// Compare eval at WHNF instead of normal form.
    pub fn whnf_eval(mut self) -> Self {
        self.deep_force_eval = false;
        self
    }

    /// The caller labels the watchdog itself; do not arm it per expression.
    pub fn caller_labels_watchdog(mut self) -> Self {
        self.watchdog = false;
        self
    }
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// A JIT failure, reduced to a nameable class so a policy can allow-list it.
/// `Other` is a class like any other: never accepted unless a lane names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitErrorClass {
    /// Nursery exhausted after GC — the legitimate tiny-nursery outcome.
    HeapOverflow,
    /// Host/JIT stack exhausted by deep non-tail recursion.
    StackOverflow,
    /// A thunk forced itself.
    BlackHole,
    /// A `VarId` reached the runtime unbound.
    UnresolvedVar,
    /// Scrutinee constructor matched no alternative, or a shape/arity guard fired.
    CaseTrap,
    /// A bad pointer was observed in the JIT runtime.
    BadPointer,
    /// Integer division by zero.
    DivisionByZero,
    /// Arithmetic overflow guard.
    Overflow,
    /// Haskell `error` (with or without a message).
    UserError,
    /// Haskell `undefined` forced.
    Undefined,
    /// Application of a null function pointer.
    NullFunPtr,
    /// Application of a non-closure.
    BadFunPtrTag,
    /// A thunk in an invalid evaluation state.
    BadThunkState,
    /// Type metadata forced.
    TypeMetadata,
    /// External cancellation.
    Cancelled,
    /// Heap-to-`Value` bridging failed (e.g. an unexpected heap tag).
    HeapBridge,
    /// Cranelift emission failed.
    Compilation,
    /// The codegen pipeline failed.
    Pipeline,
    /// Effect dispatch failed.
    Effect,
    /// A signal was caught by the JIT's signal protection.
    Signal,
    /// A `RuntimeError` or `JitError` variant with no dedicated class here.
    Other,
}

/// An eval failure, reduced to a nameable class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalErrorClass {
    /// Variable not found in the environment.
    UnboundVar,
    /// Wrong number of arguments or case binders.
    ArityMismatch,
    /// A value of the wrong shape reached a primop or application.
    TypeMismatch,
    /// No case alternative matched.
    NoMatchingAlt,
    /// A thunk forced itself.
    InfiniteLoop,
    /// The tree-walker has no implementation for this primop.
    UnsupportedPrimOp,
    /// Application of a non-function.
    NotAFunction,
    /// Jump to an unbound join point.
    UnboundJoin,
    /// Haskell `error`.
    UserError,
    /// Haskell `undefined`.
    Undefined,
    /// `deep_force` hit the recursion depth limit.
    DepthLimit,
    /// An internal invariant violation (including a leaked control signal).
    InternalError,
}

/// Reduce a [`JitError`] to its class. Total: every variant maps.
pub fn classify_jit(e: &JitError) -> JitErrorClass {
    match e {
        JitError::Compilation(_) => JitErrorClass::Compilation,
        JitError::Pipeline(_) => JitErrorClass::Pipeline,
        JitError::MissingConTags(_) => JitErrorClass::Other,
        JitError::Effect(_) => JitErrorClass::Effect,
        JitError::Yield(y) => classify_yield(y),
        JitError::HeapBridge(_) => JitErrorClass::HeapBridge,
        JitError::Signal(_) => JitErrorClass::Signal,
        JitError::EffectResponseTooLarge { .. } => JitErrorClass::Other,
        JitError::InvalidSuspensionState(_)
        | JitError::UnknownContinuation(_)
        | JitError::UnknownValueHandle(_)
        | JitError::EmptyProjection => JitErrorClass::Other,
        JitError::VarIdCollision(_) => JitErrorClass::Other,
    }
}

/// Reduce a [`YieldError`] to its class. Total: every variant maps.
fn classify_yield(e: &YieldError) -> JitErrorClass {
    match e {
        YieldError::UnexpectedTag(_) => JitErrorClass::Other,
        YieldError::UnexpectedConTag(_) => JitErrorClass::Other,
        YieldError::BadValFields(_) => JitErrorClass::Other,
        YieldError::BadEFields(_) => JitErrorClass::Other,
        YieldError::BadUnionFields(_) => JitErrorClass::Other,
        YieldError::NullPointer => JitErrorClass::Other,
        YieldError::Signal(_) => JitErrorClass::Signal,
        YieldError::Runtime(rt) => classify_runtime(rt),
    }
}

/// Reduce a [`RuntimeError`] to its class. Total: every variant maps.
fn classify_runtime(e: &RuntimeError) -> JitErrorClass {
    match e {
        RuntimeError::DivisionByZero => JitErrorClass::DivisionByZero,
        RuntimeError::Overflow => JitErrorClass::Overflow,
        RuntimeError::UserError | RuntimeError::UserErrorMsg(_) => JitErrorClass::UserError,
        RuntimeError::Undefined => JitErrorClass::Undefined,
        RuntimeError::CaseTrap => JitErrorClass::CaseTrap,
        RuntimeError::BadPointer => JitErrorClass::BadPointer,
        // A named kind-4 poison is the same fault as the anonymous one (the
        // extract resolved its identity slot), so it classifies identically —
        // naming the symbol must not move a differential comparison.
        RuntimeError::TypeMetadata | RuntimeError::UnresolvedExternal(_) => {
            JitErrorClass::TypeMetadata
        }
        RuntimeError::UnresolvedVar(..) => JitErrorClass::UnresolvedVar,
        RuntimeError::NullFunPtr => JitErrorClass::NullFunPtr,
        RuntimeError::BadFunPtrTag(_) => JitErrorClass::BadFunPtrTag,
        RuntimeError::HeapOverflow => JitErrorClass::HeapOverflow,
        RuntimeError::StackOverflow => JitErrorClass::StackOverflow,
        RuntimeError::BlackHole => JitErrorClass::BlackHole,
        RuntimeError::BadThunkState(_) => JitErrorClass::BadThunkState,
        RuntimeError::Cancelled => JitErrorClass::Cancelled,
    }
}

/// Reduce an [`EvalError`] to its class. Total: every variant maps.
pub fn classify_eval(e: &EvalError) -> EvalErrorClass {
    match e {
        EvalError::UnboundVar(_) => EvalErrorClass::UnboundVar,
        EvalError::ArityMismatch { .. } => EvalErrorClass::ArityMismatch,
        EvalError::TypeMismatch { .. } => EvalErrorClass::TypeMismatch,
        EvalError::NoMatchingAlt => EvalErrorClass::NoMatchingAlt,
        EvalError::InfiniteLoop(_) => EvalErrorClass::InfiniteLoop,
        EvalError::UnsupportedPrimOp(_) => EvalErrorClass::UnsupportedPrimOp,
        EvalError::NotAFunction => EvalErrorClass::NotAFunction,
        EvalError::UnboundJoin(_) => EvalErrorClass::UnboundJoin,
        EvalError::UserError => EvalErrorClass::UserError,
        EvalError::Undefined => EvalErrorClass::Undefined,
        EvalError::DepthLimit => EvalErrorClass::DepthLimit,
        // A leaked `Jump`-in-flight control signal is itself the internal
        // invariant violation the `InternalError` class documents.
        EvalError::InternalError(_) | EvalError::JumpInFlight => EvalErrorClass::InternalError,
    }
}

// ---------------------------------------------------------------------------
// Recorded run
// ---------------------------------------------------------------------------

/// Everything one case produced, across every runtime configuration, plus the
/// verdict read off it. Built by [`run_case`]; asserted by [`assert_case`].
#[derive(Debug)]
pub struct CaseRun {
    /// The lane that produced this run.
    pub label: &'static str,
    /// The interpreter's result (deep-forced when `deep_force_eval`).
    pub eval: Result<Value, EvalError>,
    /// One entry per nursery size in `DiffConfig::nurseries`, in order.
    pub nurseries: Vec<NurseryRun>,
    /// The single classification of this case.
    pub verdict: Verdict,
}

/// The JIT results for one nursery size.
#[derive(Debug)]
pub struct NurseryRun {
    /// The nursery size, in bytes.
    pub nursery: usize,
    /// One entry per repeat, in order. Length is `DiffConfig::repeat_count`.
    pub runs: Vec<Result<Value, JitError>>,
    /// The fork-probe outcome for this configuration.
    pub crash: CrashStatus,
}

/// What the fork probe saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashStatus {
    /// `CrashContainment::Off`, or a platform without `fork`.
    NotProbed,
    /// The child exited without a fatal signal.
    Clean,
    /// The child was terminated by this signal.
    FatalSignal(i32),
}

/// The single classification of a case. Exactly one applies; there is no
/// wildcard.
#[derive(Debug)]
pub enum Verdict {
    /// Eval succeeded, at least one nursery produced a JIT value, every JIT
    /// value agrees with eval and with every other JIT value, every repeat
    /// agreed, and any JIT failure at some other nursery size was an allowed
    /// class. This is the only verdict that counts toward reach.
    Compared,
    /// Eval succeeded; NO nursery produced a JIT value, and every JIT failure
    /// was an allowed class. Counted, not compared.
    AcceptedJitError {
        /// The class observed at the first nursery size.
        class: JitErrorClass,
    },
    /// Eval failed with an allowed class and the JIT failed too. Counted, not
    /// compared.
    AcceptedBothError {
        /// The eval failure class.
        eval: EvalErrorClass,
        /// The JIT failure class.
        jit: JitErrorClass,
    },
    /// Both engines produced a value and they disagree.
    ValueMismatch {
        /// The nursery size whose JIT value disagreed.
        nursery: usize,
    },
    /// Two nursery sizes both produced values and they disagree — the GC is
    /// not nursery-invariant.
    NurseryVariance {
        /// The first size.
        a: usize,
        /// The second, disagreeing, size.
        b: usize,
    },
    /// Two repeats at the SAME nursery size disagree, or one errored and the
    /// other did not.
    NonDeterministic {
        /// The nursery size at which the repeats disagreed.
        nursery: usize,
    },
    /// Eval succeeded, the JIT failed with a class this lane does not allow.
    JitOnlyFailure {
        /// The nursery size at which the JIT failed.
        nursery: usize,
        /// The disallowed class.
        class: JitErrorClass,
    },
    /// The JIT produced a value and eval did not — an oracle bug.
    EvalOnlyFailure {
        /// The eval failure class.
        class: EvalErrorClass,
    },
    /// Eval failed with a class this lane does not allow (the JIT failed too).
    UnexpectedEvalError {
        /// The eval failure class.
        eval: EvalErrorClass,
        /// The JIT failure class.
        jit: JitErrorClass,
    },
    /// The fork probe saw a fatal signal.
    Crash {
        /// The nursery size that faulted.
        nursery: usize,
        /// The terminating signal.
        signal: i32,
    },
}

impl Verdict {
    /// Does this verdict fail the case?
    pub fn is_failure(&self) -> bool {
        !matches!(
            self,
            Verdict::Compared
                | Verdict::AcceptedJitError { .. }
                | Verdict::AcceptedBothError { .. }
        )
    }

    /// Did this case reach a JIT-vs-eval value comparison?
    pub fn is_compared(&self) -> bool {
        matches!(self, Verdict::Compared)
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Execute one case: eval once, the JIT once per nursery × repeat, the fork
/// probe once per nursery. Classifies the result. Never asserts.
pub fn run_case(expr: &CoreExpr, table: &DataConTable, cfg: &DiffConfig) -> CaseRun {
    let _guard = cfg.watchdog.then(|| {
        crate::watchdog::arm();
        let label: String = format!("{expr:?}").chars().take(2000).collect();
        crate::watchdog::begin(&label)
    });

    let mut heap_eval = VecHeap::new();
    let env_eval = env_from_datacon_table(table);
    let eval_result = eval(expr, &env_eval, &mut heap_eval);
    let eval_result = if cfg.deep_force_eval {
        eval_result.and_then(|v| deep_force(v, &mut heap_eval))
    } else {
        eval_result
    };

    let mut nurseries = Vec::with_capacity(cfg.nurseries.len());
    for &nursery in cfg.nurseries {
        let crash = match cfg.crash_containment {
            CrashContainment::ForkProbe => fork_probe(expr, table, nursery),
            CrashContainment::Off => CrashStatus::NotProbed,
        };
        let mut runs = Vec::with_capacity(cfg.repeat_count);
        for _ in 0..cfg.repeat_count {
            let run = match JitEffectMachine::compile(expr, table, nursery) {
                Ok(mut machine) => machine.run_pure(),
                Err(e) => Err(e),
            };
            runs.push(run);
        }
        nurseries.push(NurseryRun {
            nursery,
            runs,
            crash,
        });
    }

    let verdict = classify_run(&eval_result, &nurseries, &cfg.expected);

    CaseRun {
        label: cfg.label,
        eval: eval_result,
        nurseries,
        verdict,
    }
}

/// Fork a child, compile+run one nursery configuration in it, and report
/// whether it died to a fatal signal. Unix-only; a non-unix build (or a
/// `pipe`/`fork` failure) reports [`CrashStatus::NotProbed`] rather than
/// silently skipping the probe under a misleading `Clean`.
#[cfg(unix)]
fn fork_probe(expr: &CoreExpr, table: &DataConTable, nursery: usize) -> CrashStatus {
    use std::io::Read;

    let mut fds = [0i32; 2];
    // SAFETY: pipe with a valid 2-int array.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        return CrashStatus::NotProbed;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: fork in a single-threaded test process; the child only touches
    // its own JIT state and the write end of the pipe, then _exit.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            libc::close(read_fd);
        }
        if let Ok(mut machine) = JitEffectMachine::compile(expr, table, nursery) {
            let _ = machine.run_pure();
        }
        let ok: u8 = 1;
        unsafe {
            libc::write(write_fd, &ok as *const u8 as *const libc::c_void, 1);
            libc::close(write_fd);
            libc::_exit(0);
        }
    }

    unsafe {
        libc::close(write_fd);
    }
    // SAFETY: read_fd is a valid, freshly-opened pipe read end owned by this
    // process; wrapping it in a File takes ownership for the RAII close.
    let mut f = unsafe { <std::fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(read_fd) };
    let mut buf = [0u8; 1];
    let _ = f.read(&mut buf);
    drop(f);

    let mut status: libc::c_int = 0;
    // SAFETY: waitpid on the child we just forked.
    unsafe {
        libc::waitpid(pid, &mut status as *mut libc::c_int, 0);
    }
    if libc::WIFSIGNALED(status) {
        CrashStatus::FatalSignal(libc::WTERMSIG(status))
    } else {
        CrashStatus::Clean
    }
}

#[cfg(not(unix))]
fn fork_probe(_expr: &CoreExpr, _table: &DataConTable, _nursery: usize) -> CrashStatus {
    CrashStatus::NotProbed
}

/// Read off the single [`Verdict`] a recorded [`CaseRun`] resolves to, in
/// precedence order: a fatal signal always wins; non-determinism at a
/// nursery size is checked next (it makes any single JIT result at that size
/// unreliable to compare); then the eval outcome decides which branch
/// applies.
fn classify_run(
    eval: &Result<Value, EvalError>,
    nurseries: &[NurseryRun],
    expected: &ExpectedErrorPolicy,
) -> Verdict {
    for n in nurseries {
        if let CrashStatus::FatalSignal(signal) = n.crash {
            return Verdict::Crash {
                nursery: n.nursery,
                signal,
            };
        }
    }

    for n in nurseries {
        let mut runs = n.runs.iter();
        let Some(first) = runs.next() else {
            continue;
        };
        for other in runs {
            let disagree = match (first, other) {
                (Ok(v1), Ok(v2)) => !values_equal(v1, v2),
                (Ok(_), Err(_)) | (Err(_), Ok(_)) => true,
                (Err(_), Err(_)) => false,
            };
            if disagree {
                return Verdict::NonDeterministic { nursery: n.nursery };
            }
        }
    }

    // Every repeat at a given nursery agrees (checked above), so the first
    // repeat is representative of that nursery's outcome.
    let reps: Vec<(usize, &Result<Value, JitError>)> =
        nurseries.iter().map(|n| (n.nursery, &n.runs[0])).collect();

    match eval {
        Ok(eval_val) => {
            let ok_vals: Vec<(usize, &Value)> = reps
                .iter()
                .filter_map(|(n, r)| r.as_ref().ok().map(|v| (*n, v)))
                .collect();
            let err_entries: Vec<(usize, JitErrorClass)> = reps
                .iter()
                .filter_map(|(n, r)| r.as_ref().err().map(|e| (*n, classify_jit(e))))
                .collect();

            if !ok_vals.is_empty() {
                for &(nursery, v) in &ok_vals {
                    if !values_equal(eval_val, v) {
                        return Verdict::ValueMismatch { nursery };
                    }
                }
                let (a_nursery, a_val) = ok_vals[0];
                for &(b_nursery, b_val) in &ok_vals[1..] {
                    if !values_equal(a_val, b_val) {
                        return Verdict::NurseryVariance {
                            a: a_nursery,
                            b: b_nursery,
                        };
                    }
                }
                for &(nursery, class) in &err_entries {
                    if !expected.jit.contains(&class) {
                        return Verdict::JitOnlyFailure { nursery, class };
                    }
                }
                Verdict::Compared
            } else if err_entries
                .iter()
                .all(|(_, class)| expected.jit.contains(class))
            {
                Verdict::AcceptedJitError {
                    class: err_entries[0].1,
                }
            } else {
                #[allow(
                    clippy::expect_used,
                    reason = "at least one nursery's class is not in expected.jit"
                )]
                let (nursery, class) = err_entries
                    .iter()
                    .copied()
                    .find(|(_, class)| !expected.jit.contains(class))
                    .expect("at least one nursery's class is not in expected.jit");
                Verdict::JitOnlyFailure { nursery, class }
            }
        }
        Err(eval_err) => {
            let any_jit_ok = reps.iter().any(|(_, r)| r.is_ok());
            let eval_class = classify_eval(eval_err);
            if any_jit_ok {
                return Verdict::EvalOnlyFailure { class: eval_class };
            }
            #[allow(
                clippy::expect_used,
                reason = "no nursery produced a value, so the first nursery must have errored"
            )]
            let jit_class = reps[0]
                .1
                .as_ref()
                .err()
                .map(classify_jit)
                .expect("no nursery produced a value, so the first nursery must have errored");
            if expected.eval.contains(&eval_class) {
                Verdict::AcceptedBothError {
                    eval: eval_class,
                    jit: jit_class,
                }
            } else {
                Verdict::UnexpectedEvalError {
                    eval: eval_class,
                    jit: jit_class,
                }
            }
        }
    }
}

/// Turn a recorded run's verdict into a proptest result, with a failure message
/// carrying the expression and the disagreeing values.
pub fn assert_case(run: &CaseRun, expr: &CoreExpr) -> Result<(), TestCaseError> {
    let nursery_run = |nursery: usize| run.nurseries.iter().find(|n| n.nursery == nursery);

    match &run.verdict {
        Verdict::Compared
        | Verdict::AcceptedJitError { .. }
        | Verdict::AcceptedBothError { .. } => Ok(()),
        Verdict::ValueMismatch { nursery } => {
            let jit_val = nursery_run(*nursery).and_then(|n| n.runs[0].as_ref().ok());
            Err(TestCaseError::fail(format!(
                "{}: JIT and eval results differ at nursery {} bytes.\nEval: {:?}\nJIT:  {:?}\nExpr: {:#?}",
                run.label, nursery, run.eval, jit_val, expr
            )))
        }
        Verdict::NurseryVariance { a, b } => {
            let va = nursery_run(*a).and_then(|n| n.runs[0].as_ref().ok());
            let vb = nursery_run(*b).and_then(|n| n.runs[0].as_ref().ok());
            Err(TestCaseError::fail(format!(
                "{}: JIT result varies with nursery size (GC is not nursery-invariant) — \
                 nursery {} bytes -> {:?}, nursery {} bytes -> {:?}.\nEval: {:?}\nExpr: {:#?}",
                run.label, a, va, b, vb, run.eval, expr
            )))
        }
        Verdict::NonDeterministic { nursery } => {
            let runs = nursery_run(*nursery).map(|n| &n.runs);
            Err(TestCaseError::fail(format!(
                "{}: JIT non-determinism at nursery {} bytes — repeats disagree.\nRuns: {:?}\nExpr: {:#?}",
                run.label, nursery, runs, expr
            )))
        }
        Verdict::JitOnlyFailure { nursery, class } => {
            let jit_err = nursery_run(*nursery).and_then(|n| n.runs[0].as_ref().err());
            Err(TestCaseError::fail(format!(
                "{}: JIT failed at nursery {} bytes with disallowed class {:?} but eval succeeded.\n\
                 Eval: {:?}\nJIT error: {:?}\nExpr: {:#?}",
                run.label, nursery, class, run.eval, jit_err, expr
            )))
        }
        Verdict::EvalOnlyFailure { class } => Err(TestCaseError::fail(format!(
            "{}: JIT produced a value but eval failed — an oracle bug (class {:?}).\n\
             Eval: {:?}\nJIT: {:?}\nExpr: {:#?}",
            run.label, class, run.eval, run.nurseries, expr
        ))),
        Verdict::UnexpectedEvalError {
            eval: eval_class,
            jit: jit_class,
        } => Err(TestCaseError::fail(format!(
            "{}: eval failed with disallowed class {:?} (JIT also failed, class {:?}).\n\
             Eval: {:?}\nExpr: {:#?}",
            run.label, eval_class, jit_class, run.eval, expr
        ))),
        Verdict::Crash { nursery, signal } => Err(TestCaseError::fail(format!(
            "{}: fatal signal {} in the forked JIT at nursery {} bytes.\nExpr: {:#?}",
            run.label, signal, nursery, expr
        ))),
    }
}

/// The lane entry point: build a synthetic `DataConTable` for `expr`, run it,
/// record reach, and assert. This is what a `proptest!` body calls.
pub fn check(expr: CoreExpr, cfg: &DiffConfig, reach: &ReachCounter) -> Result<(), TestCaseError> {
    let table = crate::proptest::build_table_for_expr(&expr);
    check_with_table(&expr, &table, cfg, reach)
}

/// [`check`] against a caller-supplied `DataConTable` — for lanes replaying real
/// captured Core, where a fabricated table would mask the very tag misreads the
/// lane exists to catch.
pub fn check_with_table(
    expr: &CoreExpr,
    table: &DataConTable,
    cfg: &DiffConfig,
    reach: &ReachCounter,
) -> Result<(), TestCaseError> {
    let run = run_case(expr, table, cfg);
    reach.record(&run.verdict);
    assert_case(&run, expr)
}

// ---------------------------------------------------------------------------
// Reach accounting
// ---------------------------------------------------------------------------

/// Counts how many cases reached a JIT-vs-eval value comparison — the
/// `minimum_comparison_reach` gate, without which a lane can go green by
/// comparing nothing.
///
/// A lane creates one per property, local to the test function that drives the
/// property, and asserts its floor after the run. It must NOT live in a
/// `static` read by a separate `#[test]`: nextest gives every test its own
/// process, so a separate floor test always sees zero.
#[derive(Debug)]
pub struct ReachCounter {
    label: &'static str,
    total: AtomicU64,
    reached: AtomicU64,
}

impl ReachCounter {
    /// A counter for one lane.
    pub const fn new(label: &'static str) -> Self {
        Self {
            label,
            total: AtomicU64::new(0),
            reached: AtomicU64::new(0),
        }
    }

    /// Record one case's verdict.
    pub fn record(&self, verdict: &Verdict) {
        self.total.fetch_add(1, Ordering::Relaxed);
        if verdict.is_compared() {
            self.reached.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Cases seen, and cases that reached value comparison.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.total.load(Ordering::Relaxed),
            self.reached.load(Ordering::Relaxed),
        )
    }

    /// Print the reach line and assert the floor. Panics when no case was
    /// recorded at all: a floor over an empty run is not a pass.
    pub fn assert_floor(&self, min_ratio: f64) {
        let (total, reached) = self.counts();
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

// ---------------------------------------------------------------------------
// The runner's own gate: every Verdict variant class must be reachable and
// correctly assigned. This module is what makes the runner a gate rather
// than a trusted-on-faith black box; it uses synthetic recorded runs, not a
// mutated production path.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::types::{Literal, VarId};
    use tidepool_repr::{CoreFrame, TreeBuilder};

    fn lit_int(n: i64) -> CoreExpr {
        let mut b = TreeBuilder::new();
        b.push(CoreFrame::Lit(Literal::LitInt(n)));
        b.build()
    }

    /// A bare reference to a `VarId` no table/env binds — a legitimate
    /// `EvalError::UnboundVar` on the eval side.
    fn unbound_var() -> CoreExpr {
        let mut b = TreeBuilder::new();
        b.push(CoreFrame::Var(VarId(999_999)));
        b.build()
    }

    fn eval_deep(expr: &CoreExpr, table: &DataConTable) -> Result<Value, EvalError> {
        let mut heap = VecHeap::new();
        let env = env_from_datacon_table(table);
        eval(expr, &env, &mut heap).and_then(|v| deep_force(v, &mut heap))
    }

    #[test]
    fn agreeing_case_is_compared() {
        let expr = lit_int(42);
        let table = DataConTable::new();
        let cfg = DiffConfig::new("test/agree");
        let run = run_case(&expr, &table, &cfg);
        assert!(
            matches!(run.verdict, Verdict::Compared),
            "expected Compared, got {:?}",
            run.verdict
        );
        assert!(assert_case(&run, &expr).is_ok());
    }

    /// A real eval result paired with a deliberately WRONG recorded JIT
    /// value (no actual JIT bug — the recorded run is hand-built) proves
    /// `classify_run` actually catches a value divergence instead of
    /// silently agreeing.
    #[test]
    fn deliberate_value_disagreement_is_a_value_mismatch() {
        let expr = lit_int(42);
        let table = DataConTable::new();
        let eval_result = eval_deep(&expr, &table);
        assert!(
            eval_result.is_ok(),
            "eval should produce a value for Lit 42"
        );
        let lying_jit_value = Ok(Value::Lit(Literal::LitInt(999)));
        let nurseries = vec![NurseryRun {
            nursery: 64 * 1024,
            runs: vec![lying_jit_value],
            crash: CrashStatus::NotProbed,
        }];
        let verdict = classify_run(&eval_result, &nurseries, &ExpectedErrorPolicy::default());
        assert!(
            matches!(verdict, Verdict::ValueMismatch { nursery } if nursery == 64 * 1024),
            "expected ValueMismatch, got {:?}",
            verdict
        );
    }

    #[test]
    fn eval_error_under_empty_policy_is_a_failure() {
        let expr = unbound_var();
        let table = DataConTable::new();
        let cfg = DiffConfig::new("test/unbound-strict");
        let run = run_case(&expr, &table, &cfg);
        assert!(
            run.verdict.is_failure(),
            "an eval error under an empty policy must be a failure (never a discard), got {:?}",
            run.verdict
        );
        assert!(assert_case(&run, &expr).is_err());
    }

    #[test]
    fn eval_error_under_named_policy_is_accepted() {
        let expr = unbound_var();
        let table = DataConTable::new();
        let cfg =
            DiffConfig::new("test/unbound-tolerant").expect_eval(&[EvalErrorClass::UnboundVar]);
        let run = run_case(&expr, &table, &cfg);
        assert!(
            matches!(
                run.verdict,
                Verdict::AcceptedBothError {
                    eval: EvalErrorClass::UnboundVar,
                    ..
                }
            ),
            "expected AcceptedBothError, got {:?}",
            run.verdict
        );
        assert!(assert_case(&run, &expr).is_ok());
    }

    #[test]
    #[should_panic(expected = "reach floor asserted over zero cases")]
    fn reach_floor_over_empty_run_panics() {
        let reach = ReachCounter::new("test/empty");
        reach.assert_floor(0.0);
    }
}
