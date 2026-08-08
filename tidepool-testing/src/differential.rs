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
use tidepool_codegen::jit_machine::JitError;
use tidepool_eval::error::EvalError;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::CoreExpr;

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
        assert!(!ns.is_empty(), "{}: nurseries must be non-empty", self.label);
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
    let _ = e;
    unimplemented!("Dev A: implement in the runner core")
}

/// Reduce an [`EvalError`] to its class. Total: every variant maps.
pub fn classify_eval(e: &EvalError) -> EvalErrorClass {
    let _ = e;
    unimplemented!("Dev A: implement in the runner core")
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
    let _ = (expr, table, cfg);
    unimplemented!("Dev A: implement in the runner core")
}

/// Turn a recorded run's verdict into a proptest result, with a failure message
/// carrying the expression and the disagreeing values.
pub fn assert_case(run: &CaseRun, expr: &CoreExpr) -> Result<(), TestCaseError> {
    let _ = (run, expr);
    unimplemented!("Dev A: implement in the runner core")
}

/// The lane entry point: build a synthetic `DataConTable` for `expr`, run it,
/// record reach, and assert. This is what a `proptest!` body calls.
pub fn check(
    expr: CoreExpr,
    cfg: &DiffConfig,
    reach: &ReachCounter,
) -> Result<(), TestCaseError> {
    let _ = (expr, cfg, reach);
    unimplemented!("Dev A: implement in the runner core")
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
    let _ = (expr, table, cfg, reach);
    unimplemented!("Dev A: implement in the runner core")
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
        let _ = (self.label, min_ratio);
        unimplemented!("Dev A: implement in the runner core")
    }
}
