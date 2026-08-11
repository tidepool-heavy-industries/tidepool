//! Shared proptest helpers for property-based testing across crates:
//! `build_table_for_expr`, `check_jit_vs_eval`, and `check_pass_preserves_eval`.

use proptest::prelude::*;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_eval::error::EvalError;
use tidepool_eval::value::Value;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, Env, VecHeap};
use tidepool_optimize::Pass;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::AltCon;
use tidepool_repr::CoreExpr;

use crate::gen::standard_datacon_table;

/// Structural comparison for proptest contexts. Un-forced synthetic expressions
/// may contain ThunkRefs, closures, and JoinConts that can't be compared
/// structurally — a pair is skipped (treated equal) when EITHER side is such an
/// incomparable kind. A Lit-vs-Con pair, however, is two COMPARABLE kinds that
/// disagree — that is a divergence, not a skip. (The old catch-all equated
/// them, so differential greens only proved agreement up to shape-class:
/// proptest_infra_selftest BUG-1.)
pub fn values_equal(a: &Value, b: &Value) -> bool {
    fn incomparable(v: &Value) -> bool {
        !matches!(v, Value::Lit(_) | Value::Con(_, _))
    }
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((x, y)) = stack.pop() {
        if incomparable(x) || incomparable(y) {
            continue; // closure/thunk/joincont on either side: skip
        }
        match (x, y) {
            (Value::Lit(l1), Value::Lit(l2)) => {
                if l1 != l2 {
                    return false;
                }
            }
            (Value::Con(tag1, fields1), Value::Con(tag2, fields2)) => {
                if tag1 != tag2 || fields1.len() != fields2.len() {
                    return false;
                }
                for pair in fields1.iter().zip(fields2.iter()) {
                    stack.push(pair);
                }
            }
            // Lit-vs-Con (either order): comparable kinds, different shapes.
            _ => return false,
        }
    }
    true
}

/// Walk the tree to find all DataConIds and their arities.
pub fn build_table_for_expr(expr: &CoreExpr) -> DataConTable {
    let mut table = standard_datacon_table();
    let mut seen = std::collections::HashMap::new();

    for node in &expr.nodes {
        match node {
            CoreFrame::Con { tag, fields } => {
                let arity = fields.len() as u32;
                let entry = seen.entry(*tag).or_insert(0);
                if arity > *entry {
                    *entry = arity;
                }
            }
            CoreFrame::Case { alts, .. } => {
                for alt in alts {
                    if let AltCon::DataAlt(tag) = alt.con {
                        let arity = alt.binders.len() as u32;
                        let entry = seen.entry(tag).or_insert(0);
                        if arity > *entry {
                            *entry = arity;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for (id, arity) in seen {
        if table.get(id).is_none() {
            table.insert(tidepool_repr::datacon::DataCon {
                id,
                name: format!("C{}", id.0),
                tag: (id.0 % 100) as u32 + 1,
                rep_arity: arity,
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            });
        }
    }

    table
}

/// Arm the per-case hang watchdog and record this case's expression as the
/// current item. A generated program that non-terminates (blackhole-class,
/// JIT/eval loop) then aborts the suite after `TIDEPOOL_FIXTURE_TIMEOUT_SECS`
/// with a truncated dump of the offending expression — enough to reconstruct
/// the case, which a killed proptest run otherwise loses (the seed dies with
/// the process).
#[must_use = "hold the returned guard for the duration of this case's processing"]
fn watchdog_this_case(expr: &CoreExpr) -> crate::watchdog::Guard {
    crate::watchdog::arm();
    let label: String = format!("{expr:?}").chars().take(2000).collect();
    crate::watchdog::begin(&label)
}

/// The legacy synthetic-IR JIT-vs-eval policy, retained ONLY for the
/// out-of-boundary `tidepool-runtime` lanes (`proptest_jit_vs_eval`,
/// `proptest_letrec`, `proptest_gc_pressure`) that still call
/// [`check_jit_vs_eval`] directly. In-boundary lanes configure
/// [`crate::differential::DiffConfig`] with a strict (empty-by-default)
/// policy instead — see that module's docs.
///
/// `tidepool-testing`'s synthetic `CoreExpr` generator (`gen::arb_core_expr`)
/// is partial by construction, not total/ground like the hand-built lanes in
/// `tidepool-codegen/tests/`: it can produce `LetRec` shapes with
/// inter-dependent simple bindings that the interpreter thunks but the JIT
/// evaluates sequentially (`UnresolvedVar`), those unresolved vars can leave
/// garbage heap objects behind a later read (`HeapBridge`), and a tiny
/// nursery can legitimately overflow (`HeapOverflow`) — the three JIT classes
/// this policy names.
///
/// Two eval classes are also named:
///
/// - `TypeMismatch`: numeric-conversion chains (e.g.
///   `tidepool-codegen/tests/proptest_numeric_conversions.rs`, an in-boundary
///   lane still on this shim) can feed an out-of-range `Int#` through `Chr`.
///   Both engines correctly REJECT that input — eval as `TypeMismatch`
///   ("valid Unicode codepoint"), the JIT as a runtime `UserError`
///   ("Prelude.chr: bad argument") — so a both-fail outcome there is the
///   generator producing invalid input, not a bug.
/// - `InfiniteLoop`: the synthetic generator can build a self-referencing
///   thunk (e.g. `tidepool-optimize/tests/optimizer_matrix/shadowing.rs`). Both
///   engines correctly detect it — eval as `InfiniteLoop` (a thunk forcing
///   itself), the JIT as the same phenomenon under its own name,
///   `JitErrorClass::BlackHole`. `BlackHole` is deliberately NOT in this
///   policy's `expect_jit` list (unlike `HeapOverflow`/`UnresolvedVar`/
///   `HeapBridge`): a JIT-only blackhole (eval succeeds, JIT loops) would
///   still be a real divergence worth catching; only the BOTH-fail shape,
///   where eval independently confirms the loop, is tolerated here.
///
/// No other eval class is named: eval failing on this generator is otherwise
/// always a failure, not a skip.
fn legacy_synthetic_policy(label: &'static str) -> crate::differential::DiffConfig {
    use crate::differential::{DiffConfig, EvalErrorClass, JitErrorClass};
    DiffConfig::new(label)
        .expect_jit(&[
            JitErrorClass::HeapOverflow,
            JitErrorClass::UnresolvedVar,
            JitErrorClass::HeapBridge,
        ])
        .expect_eval(&[EvalErrorClass::TypeMismatch, EvalErrorClass::InfiniteLoop])
}

/// Compare JIT and interpreter results for a given expression, under the
/// [`legacy_synthetic_policy`]. A thin shim over
/// [`crate::differential::check`] — see that module's docs for the runner
/// this drives.
pub fn check_jit_vs_eval(expr: CoreExpr, nursery_size: usize) -> Result<(), TestCaseError> {
    use crate::differential::{check, ReachCounter};

    // `DiffConfig::nurseries` wants a `&'static [usize]`, but callers pass
    // `nursery_size` at runtime; a one-element leak is negligible next to a
    // proptest run's own allocation volume and keeps this a thin shim rather
    // than a config type built for a runtime-sized nursery list.
    let nurseries: &'static [usize] = Box::leak(vec![nursery_size].into_boxed_slice());
    let cfg = legacy_synthetic_policy("check_jit_vs_eval (legacy synthetic-IR shim)")
        .nurseries(nurseries);
    static REACH: ReachCounter = ReachCounter::new("check_jit_vs_eval (legacy synthetic-IR shim)");
    check(expr, &cfg, &REACH)
}

/// The full, UN-SKIPPED classification of one JIT-vs-eval run. Unlike
/// [`check_jit_vs_eval`] (which `prop_assume!`-skips both-fail and several
/// synthetic-IR-only JIT failures), every case is an explicit, inspectable
/// outcome — required for a captured REAL-Core corpus where eval-failure and
/// both-fail are FINDINGS, not noise to swallow.
#[derive(Debug)]
pub enum CapturedOutcome {
    /// Both engines succeeded and agree — no bug.
    Agree(Value),
    /// Both succeeded but produced different values — a value bug (either side).
    Diverge { eval: Value, jit: Value },
    /// Eval succeeded, JIT failed — the canonical JIT-only bug (e.g. #1
    /// roundingMode#). The captured differential's primary signal.
    JitOnlyFailure { eval: Value, jit: JitError },
    /// JIT succeeded, eval failed — an interpreter/tree-walker bug.
    EvalOnlyFailure { eval: EvalError, jit: Value },
    /// BOTH failed — a shared/translation bug (e.g. #2 read): NOT a JIT-vs-eval
    /// differential, so the differential oracle structurally cannot catch it.
    BothFail { eval: EvalError, jit: JitError },
}

/// Strict JIT-vs-eval differential for REAL captured Core, run against the
/// extractor's `meta.cbor` `DataConTable` (NOT the synthetic
/// `build_table_for_expr`, which fabricates tags — fatal here, since the bugs
/// ARE constructor tag/field misreads). Classifies the run into a
/// [`CapturedOutcome`] with NO silent skips.
pub fn check_jit_vs_eval_captured(
    expr: &CoreExpr,
    table: &DataConTable,
    nursery_size: usize,
) -> CapturedOutcome {
    // No watchdog_this_case here: the corpus drivers that call this label the
    // watchdog with the FIXTURE NAME before each call, which identifies a hang
    // better than an expression dump would.
    let mut heap_eval = VecHeap::new();
    let env_eval = env_from_datacon_table(table);
    let res_eval = eval(expr, &env_eval, &mut heap_eval);

    let res_jit = match JitEffectMachine::compile(expr, table, nursery_size) {
        Ok(mut machine) => machine.run_pure(),
        Err(e) => Err(e),
    };

    match (res_eval, res_jit) {
        (Ok(ev), Ok(jit)) => {
            if values_equal(&ev, &jit) {
                CapturedOutcome::Agree(ev)
            } else {
                CapturedOutcome::Diverge { eval: ev, jit }
            }
        }
        (Ok(ev), Err(jit)) => CapturedOutcome::JitOnlyFailure { eval: ev, jit },
        (Err(ev), Ok(jit)) => CapturedOutcome::EvalOnlyFailure { eval: ev, jit },
        (Err(ev), Err(jit)) => CapturedOutcome::BothFail { eval: ev, jit },
    }
}

/// Verify an optimization pass preserves evaluation results.
///
/// Evaluates the expression before and after the pass, deep-forces both
/// results to normal form, then structurally compares them. Deep-forcing
/// matters here: a pass bug that corrupts a value under a lazy constructor
/// field (e.g. a let-bound `Just x` thunk) is invisible to a WHNF-only
/// comparison — `check_jit_vs_eval` deep-forces for exactly this reason
/// (#336), and this oracle does the same. If the original
/// evaluation fails, the test case is skipped (passes only preserve behavior
/// of well-defined programs). Same policy as `cbor_roundtrip_preserves_eval`
/// (`gen/strategy.rs`): skip when BOTH deep-forces fail (e.g. a non-terminating
/// lazy field neither side can force), fail when only one does.
pub fn check_pass_preserves_eval(pass: &dyn Pass, expr: CoreExpr) -> Result<(), TestCaseError> {
    let _guard = watchdog_this_case(&expr);
    let mut heap1 = VecHeap::new();
    let env = Env::new();

    // Evaluate original
    let original_res = eval(&expr, &env, &mut heap1);

    // Run the pass
    let mut optimized = expr.clone();
    pass.run(&mut optimized);

    let mut heap2 = VecHeap::new();
    // Evaluate optimized
    let optimized_res = eval(&optimized, &env, &mut heap2);

    match (original_res, optimized_res) {
        (Ok(v1), Ok(v2)) => {
            let f1 = deep_force(v1, &mut heap1);
            let f2 = deep_force(v2, &mut heap2);
            match (f1, f2) {
                (Ok(fv1), Ok(fv2)) => {
                    prop_assert!(
                        values_equal(&fv1, &fv2),
                        "Evaluation results differ after pass {}.
Original: {:?}
Optimized: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                        pass.name(),
                        fv1,
                        fv2,
                        expr,
                        optimized
                    );
                }
                (Err(_), Err(_)) => {
                    // Both sides bottom under deep_force (e.g. a lazy field
                    // neither program forces to a value) — skip.
                }
                (Ok(_), Err(e)) => {
                    prop_assert!(
                        false,
                        "Optimized result deep_force failed but original succeeded.
Pass: {}
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                        pass.name(),
                        e,
                        expr,
                        optimized
                    );
                }
                (Err(e), Ok(_)) => {
                    prop_assert!(
                        false,
                        "Original result deep_force failed but optimized succeeded.
Pass: {}
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                        pass.name(),
                        e,
                        expr,
                        optimized
                    );
                }
            }
        }
        (Err(_), _) => {
            // If original eval fails, we skip this case.
            // Passes are only guaranteed to preserve behavior of well-defined programs.
        }
        (Ok(_), Err(e)) => {
            prop_assert!(
                false,
                "Optimized evaluation failed but original succeeded.
Pass: {}
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                pass.name(),
                e,
                expr,
                optimized
            );
        }
    }
    Ok(())
}
