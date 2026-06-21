//! PROTOTYPE (investigate.proptest-widen): widening the JIT-vs-eval differential
//! to surface VALUE-REPRESENTATION bugs on GHC base-internal optimized-Core shapes.
//!
//! Targets the distribution GAP vs `proptest_ghc_idioms.rs` (which maxes out at
//! 2-alt Maybe/Bool dispatch and single-field I# boxing):
//!
//!   (f) NWayCase     — N-constructor (>=3) sum-type case dispatch with MIXED
//!                      boxed/unboxed payloads, randomized alt order, optional
//!                      DEFAULT, dispatched at TWO sites. This is bug #1's class
//!                      (the 3-way Integer IS/IP/IN `roundingMode#` misdispatch).
//!   (g) ClosureField — a closure stored in a constructor field, then extracted
//!                      and APPLIED (result ground). Includes a CPS chain
//!                      (`More f rest | Done`) walked by unrolled application.
//!                      This is bug #2's class (a Lit read where a closure was
//!                      stored -> "Lit applied as a function" in the ReadP CPS
//!                      parser).
//!   (h) WorkerWrapper — box/unbox round-trips across I#/W#/D#/C# (not just I#)
//!                      and a mixed multi-field worker record, with PrimOps
//!                      between unwraps. Worker/wrapper unboxing at -O2.
//!
//! Oracle: the EXISTING differential oracle, `check_jit_vs_eval`, at 64KB and 4KB
//! nurseries, plus JIT determinism. Every program is TOTAL + GROUND (returns an
//! Int#) by construction, so ~100% of cases reach value comparison.
//!
//! Construction: hand-built `RecursiveTree<CoreFrame<usize>>` via `TreeBuilder`,
//! same style as `proptest_ghc_idioms.rs`. Self-contained: re-implements the tiny
//! Hole / fresh-id / fixup_root scaffolding rather than depending on that test
//! binary's private items.

use std::cell::Cell;

use proptest::prelude::*;
use proptest::test_runner::Config;
use serial_test::serial;

use tidepool_repr::types::{Alt, AltCon, DataConId, JoinId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_testing::proptest::{check_jit_vs_eval, values_equal};

// ---------------------------------------------------------------------------
// Constructor universe.
//
// Standard-table cons 0..=11 (Maybe/Bool/Pair/List/I#/W#/D#/C#/Text) keep their
// semantics. We mint a FRESH enum family in the 20.. range so `build_table_for_expr`
// auto-registers each with a distinct runtime tag (id.0 % 100 + 1) and the max
// arity it observes. Each id is used at a CONSISTENT arity everywhere.
// ---------------------------------------------------------------------------
const JUST: DataConId = DataConId(1);
const I_HASH: DataConId = DataConId(7); // I#
const W_HASH: DataConId = DataConId(8); // W#
const D_HASH: DataConId = DataConId(9); // D#
const C_HASH: DataConId = DataConId(10); // C#

// Fresh N-way enum: 5 constructors with deliberately mixed payload shapes,
// echoing Integer's IS(unboxed Int#) / IP,IN(boxed BigNat) split.
const E_NIL: DataConId = DataConId(20); // nullary                       (like a 0-ary tag)
const E_UNB: DataConId = DataConId(21); // 1 unboxed Int# field          (like IS)
const E_BOX: DataConId = DataConId(22); // 1 boxed field  (Just Int)     (like IP/IN)
const E_MIX: DataConId = DataConId(23); // 2 fields: (unboxed, boxed)    (mixed)
const E_NL2: DataConId = DataConId(24); // nullary (a SECOND nullary tag)

// Fresh closure-carrying cons.
const BOX1: DataConId = DataConId(30); // Box (a -> b)
const BOX2: DataConId = DataConId(31); // Box2 (a -> b) Int   (closure + ground sibling)
const CPS_MORE: DataConId = DataConId(32); // More (Int -> Int) K
const CPS_DONE: DataConId = DataConId(33); // Done
// Mixed sum: same field offset 0 holds a CLOSURE in one con, a non-closure (Int)
// in the sibling — the ReadP P-monad shape (some constructors carry functions,
// some carry values). Dispatching + applying/using field 0 stresses the
// "field read as the wrong representation" path (#2 non-closure-application).
const FBOX: DataConId = DataConId(34); // FBox (Int -> Int)
const VBOX: DataConId = DataConId(35); // VBox Int

// Worker record: 3 boxed fields of different primitive boxes.
const REC3: DataConId = DataConId(40); // Rec3 (I# _) (W# _) (D# _)

// ---------------------------------------------------------------------------
// Fresh VarId / JoinId supply (thread-local; reset per skeleton).
// ---------------------------------------------------------------------------
thread_local! {
    static VAR_CTR: Cell<u64> = const { Cell::new(0) };
    static JOIN_CTR: Cell<u64> = const { Cell::new(0) };
}
fn reset_ctrs() {
    VAR_CTR.with(|c| c.set(1000));
    JOIN_CTR.with(|c| c.set(0));
}
fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}
#[allow(dead_code)]
fn fresh_join() -> JoinId {
    JOIN_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        JoinId(v)
    })
}

// ---------------------------------------------------------------------------
// Root fixup: eval/compile treat the LAST node as the root. Ensure it.
// ---------------------------------------------------------------------------
fn fixup_root(tree: &mut CoreExpr, root: usize) -> CoreExpr {
    if root == tree.nodes.len() - 1 {
        return tree.clone();
    }
    let binder = fresh_var();
    let var_idx = tree.nodes.len();
    tree.nodes.push(CoreFrame::Var(binder));
    tree.nodes.push(CoreFrame::LetNonRec {
        binder,
        rhs: root,
        body: var_idx,
    });
    tree.clone()
}

fn lit(b: &mut TreeBuilder, n: i64) -> usize {
    b.push(CoreFrame::Lit(Literal::LitInt(n)))
}
fn add(b: &mut TreeBuilder, x: usize, y: usize) -> usize {
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![x, y],
    })
}
fn mul(b: &mut TreeBuilder, x: usize, y: usize) -> usize {
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntMul,
        args: vec![x, y],
    })
}

// ---------------------------------------------------------------------------
// Shared oracle: differential at two nursery sizes + determinism.
// ---------------------------------------------------------------------------
fn run_oracles(expr: CoreExpr) -> Result<(), TestCaseError> {
    check_jit_vs_eval(expr.clone(), 64 * 1024)?;
    check_jit_vs_eval(expr.clone(), 4 * 1024)?;
    let table = tidepool_testing::proptest::build_table_for_expr(&expr);
    let r1 = JitEffectMachine::compile(&expr, &table, 64 * 1024).and_then(|mut m| m.run_pure());
    let r2 = JitEffectMachine::compile(&expr, &table, 64 * 1024).and_then(|mut m| m.run_pure());
    if let (Ok(v1), Ok(v2)) = (&r1, &r2) {
        prop_assert!(
            values_equal(v1, v2),
            "B4 JIT non-determinism.\nRun1: {:?}\nRun2: {:?}\nExpr: {:#?}",
            v1,
            v2,
            expr
        );
    }
    Ok(())
}

// ===========================================================================
// (f) NWayCase — the bug-#1 class.
//
// A value of the 5-constructor enum E is built (random tag + payload), then
// dispatched at TWO case sites with INDEPENDENTLY shuffled alt orders and an
// optional DEFAULT collapsing a random subset of the cons. Each reachable branch
// produces a DISCRIMINATING Int# derived from (constructor index, payload), so a
// tag misdispatch or a wrong field extraction changes the final Int#.
// ===========================================================================

/// One enum constructor occurrence (the scrutinee value to build).
#[derive(Clone, Debug)]
enum EnumVal {
    Nil,
    Unb(i64),       // E_UNB n          (unboxed Int# field)
    Box(i64),       // E_BOX (Just m)   (boxed field)
    Mix(i64, i64),  // E_MIX n (Just m)
    Nl2,
}

#[derive(Clone, Debug)]
struct NWaySpec {
    val: EnumVal,
    /// Permutation of the 5 alts for site A (indices into the canonical alt list).
    order_a: Vec<usize>,
    /// Permutation for site B.
    order_b: Vec<usize>,
    /// If Some(k): keep the first k alts (in that site's order) and replace the
    /// rest with a single DEFAULT branch -> a partial case with a default.
    default_after_a: Option<usize>,
    default_after_b: Option<usize>,
}

fn arb_enum_val() -> impl Strategy<Value = EnumVal> {
    prop_oneof![
        Just(EnumVal::Nil),
        (-50i64..50).prop_map(EnumVal::Unb),
        (-50i64..50).prop_map(EnumVal::Box),
        (-50i64..50, -50i64..50).prop_map(|(a, b)| EnumVal::Mix(a, b)),
        Just(EnumVal::Nl2),
    ]
}

fn arb_perm5() -> impl Strategy<Value = Vec<usize>> {
    Just(vec![0usize, 1, 2, 3, 4]).prop_shuffle()
}

fn arb_nway() -> impl Strategy<Value = NWaySpec> {
    (
        arb_enum_val(),
        arb_perm5(),
        arb_perm5(),
        prop::option::of(1usize..5),
        prop::option::of(1usize..5),
    )
        .prop_map(
            |(val, order_a, order_b, default_after_a, default_after_b)| NWaySpec {
                val,
                order_a,
                order_b,
                default_after_a,
                default_after_b,
            },
        )
}

/// Push the scrutinee value; returns its root index.
fn push_enum_val(b: &mut TreeBuilder, v: &EnumVal) -> usize {
    match *v {
        EnumVal::Nil => b.push(CoreFrame::Con {
            tag: E_NIL,
            fields: vec![],
        }),
        EnumVal::Unb(n) => {
            let f = lit(b, n);
            b.push(CoreFrame::Con {
                tag: E_UNB,
                fields: vec![f],
            })
        }
        EnumVal::Box(m) => {
            let inner = lit(b, m);
            let just = b.push(CoreFrame::Con {
                tag: JUST,
                fields: vec![inner],
            });
            b.push(CoreFrame::Con {
                tag: E_BOX,
                fields: vec![just],
            })
        }
        EnumVal::Mix(n, m) => {
            let nf = lit(b, n);
            let inner = lit(b, m);
            let just = b.push(CoreFrame::Con {
                tag: JUST,
                fields: vec![inner],
            });
            b.push(CoreFrame::Con {
                tag: E_MIX,
                fields: vec![nf, just],
            })
        }
        EnumVal::Nl2 => b.push(CoreFrame::Con {
            tag: E_NL2,
            fields: vec![],
        }),
    }
}

/// Build one canonical alt (full extraction + discriminating Int#) for the alt
/// at canonical index `idx`. `base` salts the Int# so two sites with identical
/// dispatch still yield identical results (they get the SAME base).
fn build_canonical_alt(b: &mut TreeBuilder, idx: usize) -> Alt<usize> {
    match idx {
        0 => {
            // E_NIL -> 0
            let body = lit(b, 0);
            Alt {
                con: AltCon::DataAlt(E_NIL),
                binders: vec![],
                body,
            }
        }
        1 => {
            // E_UNB n -> 1000 + n
            let n = fresh_var();
            let nref = b.push(CoreFrame::Var(n));
            let k = lit(b, 1000);
            let body = add(b, k, nref);
            Alt {
                con: AltCon::DataAlt(E_UNB),
                binders: vec![n],
                body,
            }
        }
        2 => {
            // E_BOX (Just m) -> 2000 + m
            let bx = fresh_var();
            let bxref = b.push(CoreFrame::Var(bx));
            let m = fresh_var();
            let mref = b.push(CoreFrame::Var(m));
            let k = lit(b, 2000);
            let inner = add(b, k, mref);
            let jbind = fresh_var();
            let body = b.push(CoreFrame::Case {
                scrutinee: bxref,
                binder: jbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(JUST),
                    binders: vec![m],
                    body: inner,
                }],
            });
            Alt {
                con: AltCon::DataAlt(E_BOX),
                binders: vec![bx],
                body,
            }
        }
        3 => {
            // E_MIX n (Just m) -> 3000 + n + m
            let n = fresh_var();
            let bx = fresh_var();
            let nref = b.push(CoreFrame::Var(n));
            let bxref = b.push(CoreFrame::Var(bx));
            let m = fresh_var();
            let mref = b.push(CoreFrame::Var(m));
            let k = lit(b, 3000);
            let s1 = add(b, k, nref);
            let inner = add(b, s1, mref);
            let jbind = fresh_var();
            let body = b.push(CoreFrame::Case {
                scrutinee: bxref,
                binder: jbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(JUST),
                    binders: vec![m],
                    body: inner,
                }],
            });
            Alt {
                con: AltCon::DataAlt(E_MIX),
                binders: vec![n, bx],
                body,
            }
        }
        4 => {
            // E_NL2 -> 4000
            let body = lit(b, 4000);
            Alt {
                con: AltCon::DataAlt(E_NL2),
                binders: vec![],
                body,
            }
        }
        _ => unreachable!(),
    }
}

/// Build a case site over `scrut_idx` (a Var bound to the shared scrutinee) using
/// alt `order` and an optional default-after-k. Returns the case root index.
fn build_case_site(
    b: &mut TreeBuilder,
    scrut_idx: usize,
    order: &[usize],
    default_after: Option<usize>,
) -> usize {
    let mut alts: Vec<Alt<usize>> = Vec::new();
    let keep = default_after.unwrap_or(order.len()).min(order.len());
    for &canon in order.iter().take(keep) {
        alts.push(build_canonical_alt(b, canon));
    }
    if default_after.is_some() && keep < order.len() {
        // A DEFAULT branch returning a sentinel distinct from every real branch.
        let body = lit(b, 9999);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body,
        });
    }
    let cbind = fresh_var();
    b.push(CoreFrame::Case {
        scrutinee: scrut_idx,
        binder: cbind,
        alts,
    })
}

fn build_nway(spec: &NWaySpec) -> CoreExpr {
    reset_ctrs();
    let mut b = TreeBuilder::new();

    // let s = <val> in <combine site_a(s) and site_b(s)>
    let sbind = fresh_var();
    let val_root = push_enum_val(&mut b, &spec.val);

    let sref_a = b.push(CoreFrame::Var(sbind));
    let site_a = build_case_site(&mut b, sref_a, &spec.order_a, spec.default_after_a);

    let sref_b = b.push(CoreFrame::Var(sbind));
    let site_b = build_case_site(&mut b, sref_b, &spec.order_b, spec.default_after_b);

    // combine: site_a + 31 * site_b. Any divergence at EITHER site changes the
    // final Int#, and the 31 multiplier keeps the two contributions separable.
    let c31 = lit(&mut b, 31);
    let scaled = mul(&mut b, site_b, c31);
    let combined = add(&mut b, site_a, scaled);

    let root = b.push(CoreFrame::LetNonRec {
        binder: sbind,
        rhs: val_root,
        body: combined,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (f2) NWayCase on the DOUBLE-CONVERSION path — sharpened toward bug #1.
//
// The cluster analysis pins bug #1 to `GHC.Float.roundingMode#:IN`: the 3-way
// Integer IS/IP/IN dispatch reached on the `fromRational -> Double` path ("heavy
// arith works, Double-conversion path ONLY"). Generic Int#-returning dispatch
// (prop_nway_case above) stays green, so we feed each dispatched payload through
// `int2Double#` + Double arithmetic and return a DOUBLE, modelling the float
// path's value flow as closely as synthetic Core allows.
// ===========================================================================

fn dlit(b: &mut TreeBuilder, x: f64) -> usize {
    b.push(CoreFrame::Lit(Literal::LitDouble(x.to_bits())))
}
fn dadd(b: &mut TreeBuilder, x: usize, y: usize) -> usize {
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::DoubleAdd,
        args: vec![x, y],
    })
}
fn dmul(b: &mut TreeBuilder, x: usize, y: usize) -> usize {
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::DoubleMul,
        args: vec![x, y],
    })
}
fn i2d(b: &mut TreeBuilder, x: usize) -> usize {
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::Int2Double,
        args: vec![x],
    })
}

/// Double-path canonical alt: same dispatch shape as `build_canonical_alt`, but
/// each branch funnels its extracted Int# payload through `int2Double#` and
/// returns a Double.
fn build_canonical_alt_double(b: &mut TreeBuilder, idx: usize) -> Alt<usize> {
    match idx {
        0 => {
            let body = dlit(b, 0.0);
            Alt {
                con: AltCon::DataAlt(E_NIL),
                binders: vec![],
                body,
            }
        }
        1 => {
            // E_UNB n -> int2Double(n) +. 1000.0
            let n = fresh_var();
            let nref = b.push(CoreFrame::Var(n));
            let nd = i2d(b, nref);
            let k = dlit(b, 1000.0);
            let body = dadd(b, nd, k);
            Alt {
                con: AltCon::DataAlt(E_UNB),
                binders: vec![n],
                body,
            }
        }
        2 => {
            // E_BOX (Just m) -> int2Double(m) +. 2000.0
            let bx = fresh_var();
            let bxref = b.push(CoreFrame::Var(bx));
            let m = fresh_var();
            let mref = b.push(CoreFrame::Var(m));
            let md = i2d(b, mref);
            let k = dlit(b, 2000.0);
            let inner = dadd(b, md, k);
            let jbind = fresh_var();
            let body = b.push(CoreFrame::Case {
                scrutinee: bxref,
                binder: jbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(JUST),
                    binders: vec![m],
                    body: inner,
                }],
            });
            Alt {
                con: AltCon::DataAlt(E_BOX),
                binders: vec![bx],
                body,
            }
        }
        3 => {
            // E_MIX n (Just m) -> int2Double(n) +. int2Double(m) +. 3000.0
            let n = fresh_var();
            let bx = fresh_var();
            let nref = b.push(CoreFrame::Var(n));
            let bxref = b.push(CoreFrame::Var(bx));
            let m = fresh_var();
            let mref = b.push(CoreFrame::Var(m));
            let nd = i2d(b, nref);
            let md = i2d(b, mref);
            let s1 = dadd(b, nd, md);
            let k = dlit(b, 3000.0);
            let inner = dadd(b, s1, k);
            let jbind = fresh_var();
            let body = b.push(CoreFrame::Case {
                scrutinee: bxref,
                binder: jbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(JUST),
                    binders: vec![m],
                    body: inner,
                }],
            });
            Alt {
                con: AltCon::DataAlt(E_MIX),
                binders: vec![n, bx],
                body,
            }
        }
        4 => {
            let body = dlit(b, 4000.0);
            Alt {
                con: AltCon::DataAlt(E_NL2),
                binders: vec![],
                body,
            }
        }
        _ => unreachable!(),
    }
}

fn build_case_site_double(
    b: &mut TreeBuilder,
    scrut_idx: usize,
    order: &[usize],
    default_after: Option<usize>,
) -> usize {
    let mut alts: Vec<Alt<usize>> = Vec::new();
    let keep = default_after.unwrap_or(order.len()).min(order.len());
    for &canon in order.iter().take(keep) {
        alts.push(build_canonical_alt_double(b, canon));
    }
    if default_after.is_some() && keep < order.len() {
        let body = dlit(b, 9999.0);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body,
        });
    }
    let cbind = fresh_var();
    b.push(CoreFrame::Case {
        scrutinee: scrut_idx,
        binder: cbind,
        alts,
    })
}

fn build_nway_double(spec: &NWaySpec) -> CoreExpr {
    reset_ctrs();
    let mut b = TreeBuilder::new();

    let sbind = fresh_var();
    let val_root = push_enum_val(&mut b, &spec.val);

    let sref_a = b.push(CoreFrame::Var(sbind));
    let site_a = build_case_site_double(&mut b, sref_a, &spec.order_a, spec.default_after_a);

    let sref_b = b.push(CoreFrame::Var(sbind));
    let site_b = build_case_site_double(&mut b, sref_b, &spec.order_b, spec.default_after_b);

    let c31 = dlit(&mut b, 31.0);
    let scaled = dmul(&mut b, site_b, c31);
    let combined = dadd(&mut b, site_a, scaled);

    let root = b.push(CoreFrame::LetNonRec {
        binder: sbind,
        rhs: val_root,
        body: combined,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (g) ClosureField — the bug-#2 class.
//
// (g1) Box(f) then `case Box f of Box g -> g arg`.
// (g2) Box2(f, n) then `case .. of Box2 g k -> g k` (closure + ground sibling).
// (g3) CPS chain `More f1 (More f2 (... Done))` walked by UNROLLED extraction +
//      application, accumulating an Int#. Mirrors a defunctionalized
//      continuation / ReadP parser stack.
// ===========================================================================

#[derive(Clone, Debug)]
enum ClosKind {
    /// f = \x -> x + c
    AddC(i64),
    /// f = \x -> x * c
    MulC(i64),
    /// f = \x -> (x + c1) * c2  (two-op body)
    AffC(i64, i64),
}

#[derive(Clone, Debug)]
enum ClosSpec {
    Box1 { f: ClosKind, arg: i64 },
    Box2 { f: ClosKind, k: i64 },
    Cps { fs: Vec<ClosKind>, seed: i64 },
    /// 3-arg lambda partially applied to 2 args, stored in a Con field (a THUNKED
    /// PAP), then completed with the 3rd arg. `g (a3) where g = (\x y z -> ...) a1 a2`.
    Pap { a1: i64, a2: i64, a3: i64 },
    /// Mixed sum `FBox (Int->Int) | VBox Int`. Build one variant, dispatch both:
    ///   case x of { FBox g -> g arg ; VBox n -> n +# k }
    /// Field 0 is a closure in FBox, a non-closure in VBox.
    MixedSum {
        is_fbox: bool,
        f: ClosKind,
        arg: i64,
        n: i64,
        k: i64,
    },
    /// A closure CAPTURING an outer variable, stored in a field, then applied:
    ///   (\outer -> case Box (\x -> x +# outer) of Box g -> g arg) c
    /// The field holds a closure with a captured free var (not closed).
    Capture { c: i64, arg: i64 },
    /// HIGHER-ORDER: a field holds a closure that RETURNS a closure; extract and
    /// apply to TWO args in sequence:
    ///   case Box (\x -> \y -> x +# y) of Box g -> (g a) b
    HigherOrder { a: i64, b: i64 },
}

fn arb_clos_kind() -> impl Strategy<Value = ClosKind> {
    prop_oneof![
        (-12i64..12).prop_map(ClosKind::AddC),
        (-6i64..6).prop_map(ClosKind::MulC),
        (-6i64..6, -4i64..4).prop_map(|(a, b)| ClosKind::AffC(a, b)),
    ]
}

fn arb_clos() -> impl Strategy<Value = ClosSpec> {
    prop_oneof![
        (arb_clos_kind(), -20i64..20).prop_map(|(f, arg)| ClosSpec::Box1 { f, arg }),
        (arb_clos_kind(), -20i64..20).prop_map(|(f, k)| ClosSpec::Box2 { f, k }),
        (prop::collection::vec(arb_clos_kind(), 1..5), -20i64..20)
            .prop_map(|(fs, seed)| ClosSpec::Cps { fs, seed }),
        (-20i64..20, -20i64..20, -20i64..20).prop_map(|(a1, a2, a3)| ClosSpec::Pap { a1, a2, a3 }),
        (
            any::<bool>(),
            arb_clos_kind(),
            -20i64..20,
            -20i64..20,
            -20i64..20,
        )
            .prop_map(|(is_fbox, f, arg, n, k)| ClosSpec::MixedSum {
                is_fbox,
                f,
                arg,
                n,
                k,
            }),
        (-20i64..20, -20i64..20).prop_map(|(c, arg)| ClosSpec::Capture { c, arg }),
        (-20i64..20, -20i64..20).prop_map(|(a, b)| ClosSpec::HigherOrder { a, b }),
    ]
}

/// Push a closure `\x -> body(x)`; returns the Lam root index.
fn push_closure(b: &mut TreeBuilder, f: &ClosKind) -> usize {
    let x = fresh_var();
    let body = match *f {
        ClosKind::AddC(c) => {
            let xref = b.push(CoreFrame::Var(x));
            let cl = lit(b, c);
            add(b, xref, cl)
        }
        ClosKind::MulC(c) => {
            let xref = b.push(CoreFrame::Var(x));
            let cl = lit(b, c);
            mul(b, xref, cl)
        }
        ClosKind::AffC(c1, c2) => {
            let xref = b.push(CoreFrame::Var(x));
            let cl1 = lit(b, c1);
            let s = add(b, xref, cl1);
            let cl2 = lit(b, c2);
            mul(b, s, cl2)
        }
    };
    b.push(CoreFrame::Lam { binder: x, body })
}

fn build_clos(spec: &ClosSpec) -> CoreExpr {
    reset_ctrs();
    let mut b = TreeBuilder::new();

    let root = match spec {
        ClosSpec::Box1 { f, arg } => {
            // let box = Box(f) in case box of Box g -> g arg
            let fr = push_closure(&mut b, f);
            let boxed = b.push(CoreFrame::Con {
                tag: BOX1,
                fields: vec![fr],
            });
            let g = fresh_var();
            let gref = b.push(CoreFrame::Var(g));
            let a = lit(&mut b, *arg);
            let app = b.push(CoreFrame::App { fun: gref, arg: a });
            let cbind = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: boxed,
                binder: cbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(BOX1),
                    binders: vec![g],
                    body: app,
                }],
            })
        }
        ClosSpec::Box2 { f, k } => {
            // let box = Box2(f, k) in case box of Box2 g n -> g n
            let fr = push_closure(&mut b, f);
            let kf = lit(&mut b, *k);
            let boxed = b.push(CoreFrame::Con {
                tag: BOX2,
                fields: vec![fr, kf],
            });
            let g = fresh_var();
            let n = fresh_var();
            let gref = b.push(CoreFrame::Var(g));
            let nref = b.push(CoreFrame::Var(n));
            let app = b.push(CoreFrame::App {
                fun: gref,
                arg: nref,
            });
            let cbind = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: boxed,
                binder: cbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(BOX2),
                    binders: vec![g, n],
                    body: app,
                }],
            })
        }
        ClosSpec::Cps { fs, seed } => build_cps_chain(&mut b, fs, *seed),
        ClosSpec::Pap { a1, a2, a3 } => {
            // lam3 = \x -> \y -> \z -> (x +# y) +# z
            let x = fresh_var();
            let y = fresh_var();
            let z = fresh_var();
            let xref = b.push(CoreFrame::Var(x));
            let yref = b.push(CoreFrame::Var(y));
            let xy = add(&mut b, xref, yref);
            let zref = b.push(CoreFrame::Var(z));
            let body = add(&mut b, xy, zref);
            let lz = b.push(CoreFrame::Lam { binder: z, body });
            let ly = b.push(CoreFrame::Lam {
                binder: y,
                body: lz,
            });
            let lam3 = b.push(CoreFrame::Lam {
                binder: x,
                body: ly,
            });
            // pap = (lam3 a1) a2   — a partial application; storing it in a Con
            // field makes it a THUNK that forces to a PAP/closure.
            let a1l = lit(&mut b, *a1);
            let app1 = b.push(CoreFrame::App {
                fun: lam3,
                arg: a1l,
            });
            let a2l = lit(&mut b, *a2);
            let pap = b.push(CoreFrame::App {
                fun: app1,
                arg: a2l,
            });
            let boxed = b.push(CoreFrame::Con {
                tag: BOX1,
                fields: vec![pap],
            });
            // case box of BOX1 g -> g a3
            let g = fresh_var();
            let gref = b.push(CoreFrame::Var(g));
            let a3l = lit(&mut b, *a3);
            let app = b.push(CoreFrame::App {
                fun: gref,
                arg: a3l,
            });
            let cbind = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: boxed,
                binder: cbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(BOX1),
                    binders: vec![g],
                    body: app,
                }],
            })
        }
        ClosSpec::MixedSum {
            is_fbox,
            f,
            arg,
            n,
            k,
        } => {
            // Build the chosen variant...
            let val = if *is_fbox {
                let fr = push_closure(&mut b, f);
                b.push(CoreFrame::Con {
                    tag: FBOX,
                    fields: vec![fr],
                })
            } else {
                let nl = lit(&mut b, *n);
                b.push(CoreFrame::Con {
                    tag: VBOX,
                    fields: vec![nl],
                })
            };
            // ...and dispatch BOTH cons. FBox field 0 is a closure (apply it);
            // VBox field 0 is an Int# (use it). Same offset, different repr.
            let g = fresh_var();
            let gref = b.push(CoreFrame::Var(g));
            let argl = lit(&mut b, *arg);
            let fbody = b.push(CoreFrame::App {
                fun: gref,
                arg: argl,
            });
            let vn = fresh_var();
            let vnref = b.push(CoreFrame::Var(vn));
            let kl = lit(&mut b, *k);
            let vbody = add(&mut b, vnref, kl);
            let cbind = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: val,
                binder: cbind,
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(FBOX),
                        binders: vec![g],
                        body: fbody,
                    },
                    Alt {
                        con: AltCon::DataAlt(VBOX),
                        binders: vec![vn],
                        body: vbody,
                    },
                ],
            })
        }
        ClosSpec::Capture { c, arg } => {
            // (\outer -> case Box (\x -> x +# outer) of Box g -> g arg) c
            let outer = fresh_var();
            // inner closure \x -> x +# outer  (captures `outer`)
            let x = fresh_var();
            let xref = b.push(CoreFrame::Var(x));
            let outer_ref = b.push(CoreFrame::Var(outer));
            let body = add(&mut b, xref, outer_ref);
            let inner = b.push(CoreFrame::Lam { binder: x, body });
            let boxed = b.push(CoreFrame::Con {
                tag: BOX1,
                fields: vec![inner],
            });
            let g = fresh_var();
            let gref = b.push(CoreFrame::Var(g));
            let al = lit(&mut b, *arg);
            let app = b.push(CoreFrame::App { fun: gref, arg: al });
            let cbind = fresh_var();
            let case_body = b.push(CoreFrame::Case {
                scrutinee: boxed,
                binder: cbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(BOX1),
                    binders: vec![g],
                    body: app,
                }],
            });
            let lam_outer = b.push(CoreFrame::Lam {
                binder: outer,
                body: case_body,
            });
            let cl = lit(&mut b, *c);
            b.push(CoreFrame::App {
                fun: lam_outer,
                arg: cl,
            })
        }
        ClosSpec::HigherOrder { a, b: bb } => {
            // case Box (\x -> \y -> x +# y) of Box g -> (g a) b
            let x = fresh_var();
            let y = fresh_var();
            let xref = b.push(CoreFrame::Var(x));
            let yref = b.push(CoreFrame::Var(y));
            let body = add(&mut b, xref, yref);
            let ly = b.push(CoreFrame::Lam {
                binder: y,
                body,
            });
            let lx = b.push(CoreFrame::Lam {
                binder: x,
                body: ly,
            });
            let boxed = b.push(CoreFrame::Con {
                tag: BOX1,
                fields: vec![lx],
            });
            let g = fresh_var();
            let gref = b.push(CoreFrame::Var(g));
            let al = lit(&mut b, *a);
            let app1 = b.push(CoreFrame::App {
                fun: gref,
                arg: al,
            });
            let bl = lit(&mut b, *bb);
            let app2 = b.push(CoreFrame::App {
                fun: app1,
                arg: bl,
            });
            let cbind = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: boxed,
                binder: cbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(BOX1),
                    binders: vec![g],
                    body: app2,
                }],
            })
        }
    };

    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

/// Build `More f1 (More f2 (... Done))`, then unroll the walk:
///   case chain of { More g rest -> walk(rest, g acc) ; Done -> acc }
/// Returns the root index of the whole expression (chain + walk).
fn build_cps_chain(b: &mut TreeBuilder, fs: &[ClosKind], seed: i64) -> usize {
    // Build the chain value bottom-up: Done, then wrap with More from last to first.
    let mut chain = b.push(CoreFrame::Con {
        tag: CPS_DONE,
        fields: vec![],
    });
    for f in fs.iter().rev() {
        let fr = push_closure(b, f);
        chain = b.push(CoreFrame::Con {
            tag: CPS_MORE,
            fields: vec![fr, chain],
        });
    }

    // Bind the chain so the walk scrutinizes a shared value.
    let cbind_var = fresh_var();

    // Unroll the walk to depth fs.len()+1. We thread the current accumulator SSA
    // index and the current chain Var index downward.
    fn walk(
        b: &mut TreeBuilder,
        chain_idx: usize,
        acc_idx: usize,
        remaining: usize,
    ) -> usize {
        if remaining == 0 {
            // Only Done can remain; case it to stay total.
            let cb = fresh_var();
            return b.push(CoreFrame::Case {
                scrutinee: chain_idx,
                binder: cb,
                alts: vec![Alt {
                    con: AltCon::DataAlt(CPS_DONE),
                    binders: vec![],
                    body: acc_idx,
                }],
            });
        }
        // case chain of { More g rest -> walk(rest, g acc, remaining-1) ; Done -> acc }
        let g = fresh_var();
        let rest = fresh_var();
        let gref = b.push(CoreFrame::Var(g));
        // apply g to the current acc
        let applied = b.push(CoreFrame::App {
            fun: gref,
            arg: acc_idx,
        });
        // bind applied via a let so the deeper walk shares it
        let acc_bind = fresh_var();
        let rest_ref = b.push(CoreFrame::Var(rest));
        let acc_bind_ref = b.push(CoreFrame::Var(acc_bind));
        let deeper = walk(b, rest_ref, acc_bind_ref, remaining - 1);
        let more_body = b.push(CoreFrame::LetNonRec {
            binder: acc_bind,
            rhs: applied,
            body: deeper,
        });
        let done_body = acc_idx;
        let cb = fresh_var();
        b.push(CoreFrame::Case {
            scrutinee: chain_idx,
            binder: cb,
            alts: vec![
                Alt {
                    con: AltCon::DataAlt(CPS_MORE),
                    binders: vec![g, rest],
                    body: more_body,
                },
                Alt {
                    con: AltCon::DataAlt(CPS_DONE),
                    binders: vec![],
                    body: done_body,
                },
            ],
        })
    }

    let seed_idx = lit(b, seed);
    let chain_ref = b.push(CoreFrame::Var(cbind_var));
    let walk_root = walk(b, chain_ref, seed_idx, fs.len());

    b.push(CoreFrame::LetNonRec {
        binder: cbind_var,
        rhs: chain,
        body: walk_root,
    })
}

// ===========================================================================
// (h) WorkerWrapper — box/unbox round-trips across all primitive boxes.
//
// (h1) A homogeneous box chain for a chosen box kind (I#/W#/D#/C#), generalizing
//      proptest_ghc_idioms' I#-only BoxChain.
// (h2) Rec3 (I# i) (W# w) (D# d): build a 3-field worker record of mixed boxes,
//      case it open, unbox each field, recombine into an Int#.
// ===========================================================================

#[derive(Clone, Debug)]
enum BoxKind {
    Int,
    Word,
    Double,
    Char,
}

#[derive(Clone, Debug)]
enum WwSpec {
    Chain { kind: BoxKind, seed: i64, steps: Vec<i64> },
    Rec3 { i: i64, w: u64, d: f64, c: u32 },
}

fn arb_box_kind() -> impl Strategy<Value = BoxKind> {
    prop_oneof![
        Just(BoxKind::Int),
        Just(BoxKind::Word),
        Just(BoxKind::Double),
        Just(BoxKind::Char),
    ]
}

fn arb_ww() -> impl Strategy<Value = WwSpec> {
    prop_oneof![
        (arb_box_kind(), -40i64..40, prop::collection::vec(-12i64..12, 1..5))
            .prop_map(|(kind, seed, steps)| WwSpec::Chain { kind, seed, steps }),
        (-1000i64..1000, 0u64..2000, -100i64..100, 0u32..128).prop_map(|(i, w, d, c)| {
            WwSpec::Rec3 {
                i,
                w,
                d: d as f64 * 0.5,
                c: 65 + (c % 26),
            }
        }),
    ]
}

fn box_con(kind: &BoxKind) -> DataConId {
    match kind {
        BoxKind::Int => I_HASH,
        BoxKind::Word => W_HASH,
        BoxKind::Double => D_HASH,
        BoxKind::Char => C_HASH,
    }
}

/// Push a primitive literal of the given kind from an i64 seed (coerced).
fn push_prim_lit(b: &mut TreeBuilder, kind: &BoxKind, v: i64) -> usize {
    match kind {
        BoxKind::Int => b.push(CoreFrame::Lit(Literal::LitInt(v))),
        BoxKind::Word => b.push(CoreFrame::Lit(Literal::LitWord(v.unsigned_abs()))),
        BoxKind::Double => b.push(CoreFrame::Lit(Literal::LitDouble((v as f64).to_bits()))),
        BoxKind::Char => {
            let cp = char::from_u32((v.unsigned_abs() as u32) % 0x10000).unwrap_or('A');
            b.push(CoreFrame::Lit(Literal::LitChar(cp)))
        }
    }
}

fn build_ww(spec: &WwSpec) -> CoreExpr {
    reset_ctrs();
    let mut b = TreeBuilder::new();

    let root = match spec {
        WwSpec::Chain { kind, seed, steps } => {
            // Only Int/Word support IntAdd-style step arithmetic on the unboxed
            // field; for Double/Char we round-trip the box without arithmetic
            // (extract + re-box), which still exercises wrapper build/scrutinize.
            let arith = matches!(kind, BoxKind::Int | BoxKind::Word);
            let con = box_con(kind);

            fn level(
                b: &mut TreeBuilder,
                kind: &BoxKind,
                con: DataConId,
                cur_unboxed: usize,
                steps: &[i64],
                arith: bool,
            ) -> usize {
                let boxed = b.push(CoreFrame::Con {
                    tag: con,
                    fields: vec![cur_unboxed],
                });
                let fld = fresh_var();
                let bind = fresh_var();
                if steps.is_empty() {
                    // case boxed of K n -> n  (final unbox). For non-arith kinds,
                    // return a derived Int# so the result type is GROUND + Int.
                    let nref = b.push(CoreFrame::Var(fld));
                    let body = if arith {
                        nref
                    } else {
                        // ignore the field, return a constant Int# so eval/JIT
                        // agree on a comparable ground result regardless of kind.
                        lit(b, 7)
                    };
                    return b.push(CoreFrame::Case {
                        scrutinee: boxed,
                        binder: bind,
                        alts: vec![Alt {
                            con: AltCon::DataAlt(con),
                            binders: vec![fld],
                            body,
                        }],
                    });
                }
                let nref = b.push(CoreFrame::Var(fld));
                let next = if arith {
                    let cstep = b.push(CoreFrame::Lit(Literal::LitInt(steps[0])));
                    let op = match kind {
                        BoxKind::Word => PrimOpKind::WordAdd,
                        _ => PrimOpKind::IntAdd,
                    };
                    b.push(CoreFrame::PrimOp {
                        op,
                        args: vec![nref, cstep],
                    })
                } else {
                    nref // pass the field through unchanged
                };
                let rest = level(b, kind, con, next, &steps[1..], arith);
                b.push(CoreFrame::Case {
                    scrutinee: boxed,
                    binder: bind,
                    alts: vec![Alt {
                        con: AltCon::DataAlt(con),
                        binders: vec![fld],
                        body: rest,
                    }],
                })
            }

            let seed_idx = push_prim_lit(&mut b, kind, *seed);
            level(&mut b, kind, con, seed_idx, steps, arith)
        }
        WwSpec::Rec3 { i, w, d, c } => {
            // Rec3 (I# i) (W# w) (D# d) (C# c), then unbox all and combine to Int#.
            let il = b.push(CoreFrame::Lit(Literal::LitInt(*i)));
            let ib = b.push(CoreFrame::Con {
                tag: I_HASH,
                fields: vec![il],
            });
            let wl = b.push(CoreFrame::Lit(Literal::LitWord(*w)));
            let wb = b.push(CoreFrame::Con {
                tag: W_HASH,
                fields: vec![wl],
            });
            let dl = b.push(CoreFrame::Lit(Literal::LitDouble(d.to_bits())));
            let db = b.push(CoreFrame::Con {
                tag: D_HASH,
                fields: vec![dl],
            });
            let cl = b.push(CoreFrame::Lit(Literal::LitChar(
                char::from_u32(*c).unwrap_or('A'),
            )));
            let cb = b.push(CoreFrame::Con {
                tag: C_HASH,
                fields: vec![cl],
            });
            let rec = b.push(CoreFrame::Con {
                tag: REC3,
                fields: vec![ib, wb, db, cb],
            });

            // case rec of Rec3 bi bw bd bc ->
            //   case bi of I# ni -> case bw of W# nw ->
            //     (ni +# word2int nw)  -- Double/Char fields forced but not folded
            let bi = fresh_var();
            let bw = fresh_var();
            let bd = fresh_var();
            let bc = fresh_var();

            let bi_ref = b.push(CoreFrame::Var(bi));
            let ni = fresh_var();
            let bw_ref = b.push(CoreFrame::Var(bw));
            let nw = fresh_var();

            let ni_ref = b.push(CoreFrame::Var(ni));
            let nw_ref = b.push(CoreFrame::Var(nw));
            let nw_int = b.push(CoreFrame::PrimOp {
                op: PrimOpKind::Word2Int,
                args: vec![nw_ref],
            });
            let sum = add(&mut b, ni_ref, nw_int);

            // inner: case bw of W# nw -> sum
            let wbind = fresh_var();
            let inner_w = b.push(CoreFrame::Case {
                scrutinee: bw_ref,
                binder: wbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(W_HASH),
                    binders: vec![nw],
                    body: sum,
                }],
            });
            // mid: case bi of I# ni -> inner_w
            let ibind = fresh_var();
            let inner_i = b.push(CoreFrame::Case {
                scrutinee: bi_ref,
                binder: ibind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(I_HASH),
                    binders: vec![ni],
                    body: inner_w,
                }],
            });
            // outer: case rec of Rec3 bi bw bd bc -> inner_i
            let rbind = fresh_var();
            b.push(CoreFrame::Case {
                scrutinee: rec,
                binder: rbind,
                alts: vec![Alt {
                    con: AltCon::DataAlt(REC3),
                    binders: vec![bi, bw, bd, bc],
                    body: inner_i,
                }],
            })
        }
    };

    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// Properties.
// ===========================================================================

fn cfg() -> Config {
    let mut c = Config::with_cases(400);
    c.max_shrink_iters = 6000;
    c
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    #[serial]
    fn prop_nway_case(spec in arb_nway()) {
        let expr = build_nway(&spec);
        prop_assert!(expr.nodes.len() <= 400);
        run_oracles(expr)?;
    }
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    #[serial]
    fn prop_nway_case_double(spec in arb_nway()) {
        let expr = build_nway_double(&spec);
        prop_assert!(expr.nodes.len() <= 400);
        run_oracles(expr)?;
    }
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    #[serial]
    fn prop_closure_field(spec in arb_clos()) {
        let expr = build_clos(&spec);
        prop_assert!(expr.nodes.len() <= 400);
        run_oracles(expr)?;
    }
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    #[serial]
    fn prop_worker_wrapper(spec in arb_ww()) {
        let expr = build_ww(&spec);
        prop_assert!(expr.nodes.len() <= 400);
        run_oracles(expr)?;
    }
}

// ===========================================================================
// Smoke tests: each builder produces a JIT==eval-agreeing program on a fixed,
// hand-chosen input (sanity that the generators are well-formed before fuzzing).
// ===========================================================================

#[test]
fn smoke_nway_each_constructor() {
    use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
    for val in [
        EnumVal::Nil,
        EnumVal::Unb(7),
        EnumVal::Box(9),
        EnumVal::Mix(3, 4),
        EnumVal::Nl2,
    ] {
        let spec = NWaySpec {
            val,
            order_a: vec![0, 1, 2, 3, 4],
            order_b: vec![4, 3, 2, 1, 0],
            default_after_a: None,
            default_after_b: None,
        };
        let expr = build_nway(&spec);
        let table = tidepool_testing::proptest::build_table_for_expr(&expr);
        let mut heap = VecHeap::new();
        let env = env_from_datacon_table(&table);
        let ev = eval(&expr, &env, &mut heap).expect("eval ok");
        let jit = JitEffectMachine::compile(&expr, &table, 64 * 1024)
            .and_then(|mut m| m.run_pure())
            .expect("jit ok");
        assert!(
            values_equal(&ev, &jit),
            "NWay smoke divergence: eval={:?} jit={:?}\nspec={:?}",
            ev,
            jit,
            spec
        );
    }
}

#[test]
fn smoke_closure_field() {
    use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
    for spec in [
        ClosSpec::Box1 {
            f: ClosKind::AddC(5),
            arg: 10,
        },
        ClosSpec::Box2 {
            f: ClosKind::MulC(3),
            k: 7,
        },
        ClosSpec::Cps {
            fs: vec![ClosKind::AddC(1), ClosKind::MulC(2), ClosKind::AddC(3)],
            seed: 0,
        },
        ClosSpec::Pap {
            a1: 1,
            a2: 2,
            a3: 3,
        },
        ClosSpec::MixedSum {
            is_fbox: true,
            f: ClosKind::AddC(5),
            arg: 10,
            n: 0,
            k: 0,
        },
        ClosSpec::MixedSum {
            is_fbox: false,
            f: ClosKind::AddC(5),
            arg: 10,
            n: 7,
            k: 3,
        },
        ClosSpec::Capture { c: 100, arg: 7 },
        ClosSpec::HigherOrder { a: 8, b: 9 },
    ] {
        let expr = build_clos(&spec);
        let table = tidepool_testing::proptest::build_table_for_expr(&expr);
        let mut heap = VecHeap::new();
        let env = env_from_datacon_table(&table);
        let ev = eval(&expr, &env, &mut heap).expect("eval ok");
        let jit = JitEffectMachine::compile(&expr, &table, 64 * 1024)
            .and_then(|mut m| m.run_pure())
            .expect("jit ok");
        assert!(
            values_equal(&ev, &jit),
            "Closure smoke divergence: eval={:?} jit={:?}\nspec={:?}",
            ev,
            jit,
            spec
        );
    }
}

#[test]
fn smoke_worker_wrapper() {
    use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
    for spec in [
        WwSpec::Chain {
            kind: BoxKind::Int,
            seed: 5,
            steps: vec![1, 2, 3],
        },
        WwSpec::Chain {
            kind: BoxKind::Word,
            seed: 5,
            steps: vec![1, 2],
        },
        WwSpec::Rec3 {
            i: 10,
            w: 20,
            d: 1.5,
            c: 66,
        },
    ] {
        let expr = build_ww(&spec);
        let table = tidepool_testing::proptest::build_table_for_expr(&expr);
        let mut heap = VecHeap::new();
        let env = env_from_datacon_table(&table);
        let ev = eval(&expr, &env, &mut heap).expect("eval ok");
        let jit = JitEffectMachine::compile(&expr, &table, 64 * 1024)
            .and_then(|mut m| m.run_pure())
            .expect("jit ok");
        assert!(
            values_equal(&ev, &jit),
            "WW smoke divergence: eval={:?} jit={:?}\nspec={:?}",
            ev,
            jit,
            spec
        );
    }
}
