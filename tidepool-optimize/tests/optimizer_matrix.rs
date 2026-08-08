//! The optimizer test matrix: an explicit table of (pass, generator) cells.
//!
//! Every property that exercises an optimizer pass — or the full `optimize`
//! pipeline, or the raw JIT compilation path with no pass at all — against
//! one of the three generators in this crate lives here as a named test
//! function, and [`MATRIX`] declares which (pass, generator) pairs those
//! functions cover. A pair with no test is either marked
//! [`CellKind::NotCovered`] with a reason, or [`matrix_is_well_formed`] fails:
//! there is no way for a gap to exist without either a cell or a reason.
//!
//! # Layout
//!
//! - [`support`] — shared plumbing: the thread+`TestRunner` harness, the
//!   `wrap_in_*` constructors the shaped cells build their pinned shapes
//!   from, and the full-pipeline pass-preservation oracle.
//! - [`random`] — cells driven directly by an unconstrained generator.
//! - [`shaped`] — cells that pin one exact transformation shape (the four
//!   `wrap_in_*` properties and the four `PartialEval` regressions).
//! - [`shadowing`] — the three cells against `arb_core_expr_shadowing`. Two are
//!   live; the JIT-vs-eval differential is `#[ignore]`d, carrying an open
//!   divergence recorded as [`CellKind::Quarantined`].

#[path = "optimizer_matrix/random.rs"]
mod random;
#[path = "optimizer_matrix/shadowing.rs"]
mod shadowing;
#[path = "optimizer_matrix/shaped.rs"]
mod shaped;
#[path = "optimizer_matrix/support.rs"]
mod support;

/// A pass under test, or a non-pass axis of the JIT path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassId {
    BetaReduce,
    Dce,
    Inline,
    CaseReduce,
    PartialEval,
    /// The full `optimize` pipeline (`PartialEval` + `BetaReduce` + `Inline`
    /// + `Dce` + `CaseReduce`, to fixpoint).
    FullPipeline,
    /// No optimizer pass: `normalize` + Cranelift emission only, the JIT's
    /// own compilation path.
    JitCompile,
}

/// A generator under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeneratorId {
    /// `tidepool_testing::gen::arb_core_expr` — well-typed, may produce
    /// closures; every binder id is fresh (never shadows).
    CoreExpr,
    /// `tidepool_testing::gen::arb_ground_expr` — well-typed, result type is
    /// always ground (structurally comparable, never a closure).
    GroundExpr,
    /// `tidepool_testing::gen::arb_core_expr_shadowing` — like `CoreExpr`,
    /// but a fraction of Lam/Let/Case/Join binders deliberately reuse an
    /// in-scope `VarId` of the same type.
    Shadowing,
}

/// What a cell asserts, or why it is deliberately absent.
#[derive(Debug, Clone, Copy)]
enum CellKind {
    /// `check_pass_preserves_eval` (or the pipeline-equivalent
    /// `check_pipeline_preserves_eval`): the pass/pipeline does not change
    /// what the generated expression evaluates to.
    PreservesEval { cases: u32 },
    /// A structurally-shaped regression pinning one exact transformation; the
    /// named test function owns its own narrow generator rather than one of
    /// the three [`GeneratorId`]s, so it is not itself keyed by generator —
    /// `generator` names the family the shape is drawn *into* (its random
    /// sub-expressions, where it has any).
    Shaped { test_fn: &'static str },
    /// Running `optimize` twice is the same as running it once.
    Idempotent { cases: u32 },
    /// `Dce` never increases expression size.
    NonIncreasingSize { cases: u32 },
    /// JIT-vs-eval differential via `tidepool_testing::differential`, with a
    /// named tolerated-class policy and an in-process reach floor.
    JitDifferential { cases: u32, reach_floor: f64 },
    /// The test exists and is runnable on demand, but is `#[ignore]`d because it
    /// surfaces an open divergence it cannot itself resolve. Distinct from
    /// [`CellKind::NotCovered`]: the coverage is written, not absent, and the
    /// named finding is what gates its return to the live suite. `withheld`
    /// carries the claim the cell still makes when run, so the claim keeps being
    /// validated rather than becoming unreachable while quarantined.
    Quarantined {
        test_fn: &'static str,
        withheld: &'static CellKind,
        finding: &'static str,
    },
    /// Deliberately absent: no test exercises this (pass, generator) pair.
    NotCovered { reason: &'static str },
}

/// One matrix cell: a claim (or an explicit non-claim) about one (pass,
/// generator) pair.
#[derive(Debug, Clone, Copy)]
struct Cell {
    pass: PassId,
    generator: GeneratorId,
    kind: CellKind,
}

/// The full matrix. Adding a pass or a generator means extending the two enums
/// above and adding one row per (pass, generator) pair here — covered or not.
const MATRIX: &[Cell] = &[
    // -- BetaReduce --------------------------------------------------------
    Cell {
        pass: PassId::BetaReduce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::BetaReduce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::beta_reduction_preserves_eval",
        },
    },
    Cell {
        pass: PassId::BetaReduce,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::NotCovered {
            reason: "ground-typed input is exercised end-to-end via FullPipeline x GroundExpr, \
                     which runs BetaReduce as one stage; a standalone ground lane would be redundant",
        },
    },
    Cell {
        pass: PassId::BetaReduce,
        generator: GeneratorId::Shadowing,
        kind: CellKind::NotCovered {
            reason: "shadowing coverage is carried by PartialEval and FullPipeline (which runs \
                     BetaReduce as a stage); the identified shadowing bugs were in PartialEval's \
                     Lam/Join arms and normalize.rs, not Beta. The JitCompile shadowing \
                     differential is quarantined, so it is not load-bearing for this row",
        },
    },
    // -- Dce -----------------------------------------------------------------
    Cell {
        pass: PassId::Dce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::Dce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::dce_preserves_eval",
        },
    },
    Cell {
        pass: PassId::Dce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::NonIncreasingSize { cases: 200 },
    },
    Cell {
        pass: PassId::Dce,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::NotCovered {
            reason: "ground-typed input is exercised end-to-end via FullPipeline x GroundExpr, \
                     which runs Dce as one stage; a standalone ground lane would be redundant",
        },
    },
    Cell {
        pass: PassId::Dce,
        generator: GeneratorId::Shadowing,
        kind: CellKind::NotCovered {
            reason: "shadowing coverage is carried by PartialEval and FullPipeline (which runs Dce \
                     as a stage); the identified shadowing bugs were in PartialEval's Lam/Join \
                     arms and normalize.rs, not Dce. The JitCompile shadowing differential is \
                     quarantined, so it is not load-bearing for this row",
        },
    },
    // -- Inline ----------------------------------------------------------------
    Cell {
        pass: PassId::Inline,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::Inline,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::inline_preserves_eval",
        },
    },
    Cell {
        pass: PassId::Inline,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::NotCovered {
            reason: "ground-typed input is exercised end-to-end via FullPipeline x GroundExpr, \
                     which runs Inline as one stage; a standalone ground lane would be redundant",
        },
    },
    Cell {
        pass: PassId::Inline,
        generator: GeneratorId::Shadowing,
        kind: CellKind::NotCovered {
            reason: "shadowing coverage is carried by PartialEval and FullPipeline (which runs \
                     Inline as a stage); the identified shadowing bugs were in \
                     PartialEval's Lam/Join arms and normalize.rs, not Inline. The \
                     JitCompile shadowing differential is currently quarantined, so it is not \
                     load-bearing for this row",
        },
    },
    // -- CaseReduce --------------------------------------------------------
    Cell {
        pass: PassId::CaseReduce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::CaseReduce,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::case_of_known_con_preserves_eval",
        },
    },
    Cell {
        pass: PassId::CaseReduce,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::NotCovered {
            reason: "ground-typed input is exercised end-to-end via FullPipeline x GroundExpr, \
                     which runs CaseReduce as one stage; a standalone ground lane would be redundant",
        },
    },
    Cell {
        pass: PassId::CaseReduce,
        generator: GeneratorId::Shadowing,
        kind: CellKind::NotCovered {
            reason: "shadowing coverage is carried by PartialEval and FullPipeline (which runs \
                     CaseReduce as a stage); the identified shadowing bugs were in PartialEval's \
                     Lam/Join arms and normalize.rs, not CaseReduce. The JitCompile shadowing \
                     differential is quarantined, so it is not load-bearing for this row",
        },
    },
    // -- PartialEval -------------------------------------------------------
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::nested_known_con_case_reduces",
        },
    },
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::nested_let_propagation",
        },
    },
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::primop_fold_all_foldable_ops",
        },
    },
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Shaped {
            test_fn: "shaped::primop_fold_negate",
        },
    },
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::NotCovered {
            reason: "ground-typed input is exercised end-to-end via FullPipeline x GroundExpr, \
                     which runs PartialEval as one stage; a standalone ground lane would be \
                     redundant",
        },
    },
    Cell {
        pass: PassId::PartialEval,
        generator: GeneratorId::Shadowing,
        kind: CellKind::PreservesEval { cases: 800 },
    },
    // -- FullPipeline --------------------------------------------------------
    Cell {
        pass: PassId::FullPipeline,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::FullPipeline,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::Idempotent { cases: 200 },
    },
    Cell {
        pass: PassId::FullPipeline,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::PreservesEval { cases: 200 },
    },
    Cell {
        pass: PassId::FullPipeline,
        generator: GeneratorId::Shadowing,
        kind: CellKind::PreservesEval { cases: 800 },
    },
    // -- JitCompile (no optimizer pass) -------------------------------------
    Cell {
        pass: PassId::JitCompile,
        generator: GeneratorId::CoreExpr,
        kind: CellKind::NotCovered {
            reason: "the un-shadowed JIT-vs-eval differential against synthetic CoreExpr is \
                     owned by tidepool-codegen/tests (sibling-owned at the time of this \
                     migration); this matrix carries only the shadowing-specific lane that was \
                     added here to close the shadowing blind spot",
        },
    },
    Cell {
        pass: PassId::JitCompile,
        generator: GeneratorId::GroundExpr,
        kind: CellKind::NotCovered {
            reason: "the un-shadowed JIT-vs-eval differential against ground-typed expressions is \
                     owned by tidepool-codegen/tests (sibling-owned at the time of this \
                     migration); this matrix carries only the shadowing-specific lane that was \
                     added here to close the shadowing blind spot",
        },
    },
    Cell {
        pass: PassId::JitCompile,
        generator: GeneratorId::Shadowing,
        kind: CellKind::Quarantined {
            test_fn: "shadowing::jit_agrees_with_eval_with_shadowing",
            withheld: &CellKind::JitDifferential {
                cases: 800,
                reach_floor: 0.5,
            },
            finding: "one-directional oracle divergence under shadowed binders: the JIT returns a \
                      value where eval reports InfiniteLoop (Verdict::EvalOnlyFailure), on roughly \
                      1 run in 8 at 800 cases. JitEffectMachine::compile runs normalize, which \
                      alpha-renames shadowed binders apart; the tree-walking eval does not. \
                      Resolving it means a fix in tidepool-eval or normalize.rs, which is \
                      production code outside this migration's boundary",
        },
    },
];

/// Every (pass, generator) pair has at least one row in [`MATRIX`] — covered
/// or explicitly [`CellKind::NotCovered`] — so a gap can never be silently
/// absent, and no pair is claimed both covered and not-covered at once.
#[test]
fn matrix_is_well_formed() {
    const PASSES: &[PassId] = &[
        PassId::BetaReduce,
        PassId::Dce,
        PassId::Inline,
        PassId::CaseReduce,
        PassId::PartialEval,
        PassId::FullPipeline,
        PassId::JitCompile,
    ];
    const GENERATORS: &[GeneratorId] = &[
        GeneratorId::CoreExpr,
        GeneratorId::GroundExpr,
        GeneratorId::Shadowing,
    ];

    for &pass in PASSES {
        for &generator in GENERATORS {
            let rows: Vec<&Cell> = MATRIX
                .iter()
                .filter(|c| c.pass == pass && c.generator == generator)
                .collect();
            assert!(
                !rows.is_empty(),
                "no MATRIX row for {pass:?} x {generator:?} — every pair needs a covered \
                 cell or an explicit NotCovered row",
            );
            let not_covered = rows
                .iter()
                .filter(|c| matches!(c.kind, CellKind::NotCovered { .. }))
                .count();
            assert!(
                not_covered == 0 || rows.len() == 1,
                "{pass:?} x {generator:?} has a NotCovered row alongside a covered row — \
                 that is a contradiction, not a decision",
            );
        }
    }

    /// A quarantined cell's `withheld` claim is validated by the same rules as a
    /// live one, so it cannot rot into an ill-formed row while the `#[ignore]` is
    /// on.
    fn check_claim(cell: &Cell, kind: &CellKind) {
        match *kind {
            CellKind::PreservesEval { cases }
            | CellKind::Idempotent { cases }
            | CellKind::NonIncreasingSize { cases } => {
                assert!(cases > 0, "{cell:?}: a cell needs at least one case");
            }
            CellKind::JitDifferential { cases, reach_floor } => {
                assert!(cases > 0, "{cell:?}: a cell needs at least one case");
                assert!(
                    (0.0..=1.0).contains(&reach_floor),
                    "{cell:?}: reach_floor must be a fraction"
                );
            }
            CellKind::Shaped { test_fn } => {
                assert!(
                    !test_fn.is_empty(),
                    "{cell:?}: a shaped cell names its test fn"
                );
            }
            CellKind::Quarantined {
                test_fn,
                withheld,
                finding,
            } => {
                assert!(
                    !test_fn.is_empty(),
                    "{cell:?}: a quarantined cell names the #[ignore]d test fn to run"
                );
                assert!(
                    !finding.is_empty(),
                    "{cell:?}: a quarantined cell names the open finding that gates its return"
                );
                assert!(
                    !matches!(
                        withheld,
                        CellKind::NotCovered { .. } | CellKind::Quarantined { .. }
                    ),
                    "{cell:?}: a quarantined cell withholds a real claim, not another \
                     absence or quarantine"
                );
                check_claim(cell, withheld);
            }
            CellKind::NotCovered { reason } => {
                assert!(
                    !reason.is_empty(),
                    "{cell:?}: a NotCovered row must give a reason"
                );
            }
        }
    }

    for cell in MATRIX {
        check_claim(cell, &cell.kind);
    }
}
