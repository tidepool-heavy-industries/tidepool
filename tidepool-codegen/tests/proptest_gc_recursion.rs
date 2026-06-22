//! Lane: GC x recursion/allocation.
//!
//! Allocation-heavy generated programs run through the JIT-vs-eval differential
//! oracle at a RANGE of tiny nursery sizes (forcing frequent GC, multiple
//! collection cycles per program) to surface heap-forwarding / root-tracking
//! divergences that the existing depth-3 differential never exercises.
//!
//! Four allocation shapes, each TOTAL and GROUND by construction (results are
//! Int# or Con-of-Int#, ~100% reach value comparison):
//!
//!   (a) ConsSpine     — a deep `x0 : x1 : ... : []` list (DataConId 5/6),
//!                       consumed by a tail-recursive sum/length/last walk. The
//!                       spine outlives several GC cycles; a forwarding bug
//!                       corrupts the walk. Targets GC x heap-forwarding through
//!                       a long live chain.
//!   (b) WideLive      — N (8..40) simultaneously-live bindings, each an
//!                       allocated boxed Con (I# or Pair), ALL forced and folded
//!                       in the body. Every binding is a live root across a GC
//!                       that fires mid-construction. Targets GC x many-live-roots
//!                       / root enumeration.
//!   (c) BigCon        — one large nested constructor (Pair-tree of depth k, or a
//!                       single long cons-spine returned WHOLE) that approaches /
//!                       exceeds the nursery in one object, forcing GC during its
//!                       own construction and verifying every field survives the
//!                       copy. Targets GC x large constructors.
//!   (d) AccumLoop     — a self-recursive LetRec-lambda counting loop (TCO,
//!                       unbounded depth) that allocates a fresh boxed Con every
//!                       iteration (immediate garbage) while keeping a live
//!                       accumulator. Targets GC x deep recursion: many GC
//!                       cycles, one long-lived root (the accumulator) amid a
//!                       torrent of dead allocations.
//!
//! Oracle: each generated program is run through `check_jit_vs_eval` (JIT vs the
//! tree-walking interpreter) at SEVERAL nursery sizes from 64 KiB down to 2 KiB.
//! A divergence at ANY size — or a JIT result that varies BETWEEN nursery sizes
//! (the nursery is a runtime knob; a correct GC is nursery-invariant) — is a
//! reportable bug. `HeapOverflow` / `UnresolvedVar` / `HeapBridge` JIT-only
//! errors are tolerated (a 2 KiB nursery legitimately can't hold every program).
//!
//! This lane reuses `check_jit_vs_eval` / `build_table_for_expr` / `values_equal`
//! from `tidepool_testing::proptest` as the differential oracle and only adds
//! its own allocation-shape generators (no edits to shared `strategy.rs`).

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use proptest::test_runner::Config;
use serial_test::serial;

/// Host-thread stack budget for every test in this lane.
///
/// Deep cons-spines / Pair-trees are built as deeply-NESTED `CoreExpr` trees
/// (a 500-element spine is 500 nested `Con` nodes), and the tree-walking
/// interpreter (`eval`) plus the recursive `Drop` of the resulting deep `Value`
/// spine recurse on the HOST stack — the 2 MiB default test thread overflows
/// (see MEMORY "Host stack-overflow class"). This is NOT the property under
/// test (the JIT-vs-eval differential is), so we give every test a generous
/// 256 MiB host stack and run the body there. Language-level recursion in the
/// GENERATED programs is tail-recursive (LetRec-lambda loops, TCO'd by the JIT)
/// and bounded by GC, so the large stack only protects the host-side tree/Value
/// walks.
const HOST_STACK: usize = 256 * 1024 * 1024;

/// Run `f` on a thread with the large host stack, propagating panics.
fn with_big_stack<F: FnOnce() + Send + 'static>(f: F) {
    let h = std::thread::Builder::new()
        .stack_size(HOST_STACK)
        .spawn(f)
        .expect("spawn big-stack test thread");
    h.join().expect("test thread panicked");
}

use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_testing::proptest::{build_table_for_expr, check_jit_vs_eval, values_equal};

// ---------------------------------------------------------------------------
// Standard DataCon tags (must match `standard_datacon_table` in tidepool-testing).
// ---------------------------------------------------------------------------
const JUST: DataConId = DataConId(1);
const PAIR: DataConId = DataConId(4);
const NIL: DataConId = DataConId(5); // []
const CONS: DataConId = DataConId(6); // :
const I_HASH: DataConId = DataConId(7); // I# single-field Int box wrapper

// ---------------------------------------------------------------------------
// Nursery ladder.
//
// Each generated program is run at every size in this ladder through the JIT.
// 64 KiB rarely forces GC (a control), the smaller sizes force progressively
// more collection cycles. The JIT result MUST be the same at every size AND
// equal to the interpreter's result. 2 KiB legitimately overflows some
// programs (tolerated as HeapOverflow).
// ---------------------------------------------------------------------------
const NURSERY_LADDER: &[usize] = &[64 * 1024, 16 * 1024, 8 * 1024, 4 * 1024, 2 * 1024];

// ---------------------------------------------------------------------------
// Instrumentation.
// ---------------------------------------------------------------------------
static REACHED: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);

static N_CONSSPINE: AtomicU64 = AtomicU64::new(0);
static N_WIDELIVE: AtomicU64 = AtomicU64::new(0);
static N_BIGCON: AtomicU64 = AtomicU64::new(0);
static N_ACCUMLOOP: AtomicU64 = AtomicU64::new(0);
// How many cases reached value comparison at the SMALLEST nursery that did not
// overflow (a proxy for "GC actually fired and the program still completed").
static N_GC_COMPLETED_TINY: AtomicU64 = AtomicU64::new(0);

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Fresh VarId supply (per-program, thread-local).
//
// All recursion in this lane is expressed via self-recursive `LetRec` lambdas
// (NOT join points): the tree-walking interpreter — the differential oracle —
// does not support self-recursive join points (a `jump go` inside `go`'s own
// rhs is `UnboundJoin`, because `Join` captures the env *before* its binding),
// so a join-based loop would be silently SKIPPED by `check_jit_vs_eval` rather
// than compared. A `LetRec` of a curried lambda is run by BOTH engines (the JIT
// still tail-call-optimizes it, PR #154), keeping the differential live.
// ---------------------------------------------------------------------------
thread_local! {
    static VAR_CTR: Cell<u64> = const { Cell::new(0) };
}
fn reset_ctrs() {
    VAR_CTR.with(|c| c.set(1000));
}
fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}

// ---------------------------------------------------------------------------
// Root fixup: eval/compile treat the LAST node as the root. Nested helpers can
// append children after the structural parent, so guarantee the root is last by
// wrapping it in `let v = <root> in v` when needed.
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

// ---------------------------------------------------------------------------
// Shared GC-pressure oracle.
//
// Runs `check_jit_vs_eval` at every nursery size in the ladder, and ALSO checks
// the JIT is nursery-invariant: among the sizes where the JIT succeeded, every
// result must be `values_equal`. (A divergence purely between nursery sizes is a
// GC bug even if eval happens to agree with one of them.)
// ---------------------------------------------------------------------------
fn run_gc_oracle(expr: CoreExpr) -> Result<(), TestCaseError> {
    bump(&TOTAL);

    // 1. Differential at every nursery size (B1 value-diff, B2 JIT-only error).
    for &n in NURSERY_LADDER {
        check_jit_vs_eval(expr.clone(), n)?;
    }

    // 2. Nursery-invariance: collect every successful JIT result across the
    //    ladder; all must agree. Also tracks reach + tiny-nursery completion.
    let table = build_table_for_expr(&expr);
    let mut jit_results: Vec<(usize, tidepool_eval::value::Value)> = Vec::new();
    let mut tolerated_smallest: Option<usize> = None;
    for &n in NURSERY_LADDER {
        match JitEffectMachine::compile(&expr, &table, n).and_then(|mut m| m.run_pure()) {
            Ok(v) => jit_results.push((n, v)),
            Err(JitError::Yield(YieldError::Runtime(RuntimeError::HeapOverflow)))
            | Err(JitError::Yield(YieldError::Runtime(RuntimeError::UnresolvedVar(_))))
            | Err(JitError::HeapBridge(_)) => {
                tolerated_smallest = Some(n);
            }
            Err(e) => {
                // A non-tolerated JIT error at some nursery size while OTHER
                // sizes (or eval) succeed is already caught by check_jit_vs_eval
                // above for that size; record nothing here.
                let _ = e;
            }
        }
    }

    if let Some(((n0, v0), rest)) = jit_results.split_first() {
        for (n, v) in rest {
            prop_assert!(
                values_equal(v0, v),
                "JIT result varies with nursery size (GC-dependent output).\n\
                 nursery {} -> {:?}\nnursery {} -> {:?}\nExpr: {:#?}",
                n0,
                v0,
                n,
                v,
                expr
            );
        }
        bump(&REACHED);
        // The program completed at the smallest nursery in the ladder iff the
        // smallest ladder entry produced a result (i.e. GC fired & succeeded).
        if jit_results.iter().any(|(n, _)| *n <= 4 * 1024) {
            bump(&N_GC_COMPLETED_TINY);
        }
        let _ = tolerated_smallest;
    }

    Ok(())
}

// ===========================================================================
// (a) ConsSpine
//
// Build a list of `len` Int# elements as a real `(:)`/`[]` chain, bind it, and
// consume it with a tail-recursive join-point walk producing a single Int#:
//   * Sum:    accumulate the sum of all elements
//   * Length: count elements (ignores values)
//   * Last:   return the last element (or a sentinel for [])
// `len` up to ~400 so a 2-4 KiB nursery forwards the spine repeatedly during the
// walk. Tail-recursive consumer => unbounded depth, GC is the only limit.
// ===========================================================================

#[derive(Clone, Debug)]
enum SpineConsumer {
    Sum,
    Length,
    Last,
}

#[derive(Clone, Debug)]
struct ConsSpineSpec {
    elems: Vec<i64>,
    consumer: SpineConsumer,
}

fn arb_consspine() -> impl Strategy<Value = ConsSpineSpec> {
    (
        prop::collection::vec(-1_000_000i64..1_000_000, 1..400),
        prop_oneof![
            Just(SpineConsumer::Sum),
            Just(SpineConsumer::Length),
            Just(SpineConsumer::Last),
        ],
    )
        .prop_map(|(elems, consumer)| ConsSpineSpec { elems, consumer })
}

/// Build a literal cons-spine `e0 : e1 : ... : []`, return its root index.
fn push_spine(b: &mut TreeBuilder, elems: &[i64]) -> usize {
    let mut tail = b.push(CoreFrame::Con {
        tag: NIL,
        fields: vec![],
    });
    for &e in elems.iter().rev() {
        let head = b.push(CoreFrame::Lit(Literal::LitInt(e)));
        tail = b.push(CoreFrame::Con {
            tag: CONS,
            fields: vec![head, tail],
        });
    }
    tail
}

/// How the cons-arm combines the accumulator with the head element.
#[derive(Clone, Copy)]
enum Combine {
    /// acc +# h  (Sum)
    AddHead,
    /// acc +# 1  (Length)
    Inc,
    /// h         (Last)
    TakeHead,
}

/// Build a TAIL-RECURSIVE list fold as a self-recursive `LetRec` lambda — NOT a
/// join point. The tree-walking interpreter (the differential oracle) does NOT
/// support self-recursive join points (`Join`'s `JoinCont` captures the env
/// *before* the join binding, so a `jump go` inside `go`'s own rhs is
/// `UnboundJoin`). A `LetRec` of a curried lambda is the shape both engines run:
/// the interpreter via the recursive-let cycle, the JIT with TCO (PR #154).
///
///   letrec go = \acc -> \xs ->
///                 case xs of { [] -> acc ; (h:t) -> go (combine acc h) t }
///   in go <seed> <list_var>
///
/// Returns the root index (a `LetRec`).
fn push_list_fold(b: &mut TreeBuilder, list_var: VarId, seed: i64, combine: Combine) -> usize {
    let go = fresh_var();
    let acc = fresh_var();
    let xs = fresh_var();

    let xs_v = b.push(CoreFrame::Var(xs));
    let case_binder = fresh_var();
    let h = fresh_var();
    let t = fresh_var();

    let nil_body = b.push(CoreFrame::Var(acc));

    // combined value for the cons arm
    let combined = match combine {
        Combine::AddHead => {
            let av = b.push(CoreFrame::Var(acc));
            let hv = b.push(CoreFrame::Var(h));
            b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![av, hv],
            })
        }
        Combine::Inc => {
            let av = b.push(CoreFrame::Var(acc));
            let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
            b.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![av, one],
            })
        }
        Combine::TakeHead => b.push(CoreFrame::Var(h)),
    };
    // go combined t   ==  (go combined) t
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: combined,
    });
    let t_v = b.push(CoreFrame::Var(t));
    let recur = b.push(CoreFrame::App {
        fun: app1,
        arg: t_v,
    });

    let case_node = b.push(CoreFrame::Case {
        scrutinee: xs_v,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::DataAlt(NIL),
                binders: vec![],
                body: nil_body,
            },
            Alt {
                con: AltCon::DataAlt(CONS),
                binders: vec![h, t],
                body: recur,
            },
        ],
    });

    // \acc -> \xs -> <case>
    let inner_lam = b.push(CoreFrame::Lam {
        binder: xs,
        body: case_node,
    });
    let go_lam = b.push(CoreFrame::Lam {
        binder: acc,
        body: inner_lam,
    });

    // body: go <seed> <list_var>  == (go seed) list_var
    let go_b = b.push(CoreFrame::Var(go));
    let seed_lit = b.push(CoreFrame::Lit(Literal::LitInt(seed)));
    let call1 = b.push(CoreFrame::App {
        fun: go_b,
        arg: seed_lit,
    });
    let list_v = b.push(CoreFrame::Var(list_var));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: list_v,
    });

    b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    })
}

fn build_consspine(spec: &ConsSpineSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_CONSSPINE);
    let mut b = TreeBuilder::new();

    let spine = push_spine(&mut b, &spec.elems);
    let lst = fresh_var();

    let (seed, combine) = match spec.consumer {
        SpineConsumer::Sum => (0, Combine::AddHead),
        SpineConsumer::Length => (0, Combine::Inc),
        // Last: seed with the first element's value isn't needed; an empty list
        // is impossible (elems is 1..400), so the seed is always overwritten.
        SpineConsumer::Last => (0, Combine::TakeHead),
    };
    let fold = push_list_fold(&mut b, lst, seed, combine);

    // let lst = spine in <fold>
    let root = b.push(CoreFrame::LetNonRec {
        binder: lst,
        rhs: spine,
        body: fold,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (b) WideLive
//
// N (8..40) simultaneously-live bindings via a single LetNonRec nest, each an
// allocated boxed Con (I# of an Int#, or Pair of two Int#s). The body forces &
// folds EVERY binding into a final Int# sum, so all N objects are live roots
// across any GC that fires while the later bindings are being allocated.
//
// Built as a right-nested chain of LetNonRec so the interpreter and JIT see
// classic nested non-recursive lets (the common GHC shape), not a LetRec.
// ===========================================================================

#[derive(Clone, Debug)]
enum LiveCell {
    /// I# n
    Boxed(i64),
    /// (,) a b
    Pair(i64, i64),
}

#[derive(Clone, Debug)]
struct WideLiveSpec {
    cells: Vec<LiveCell>,
}

fn arb_widelive() -> impl Strategy<Value = WideLiveSpec> {
    prop::collection::vec(
        prop_oneof![
            (-1000i64..1000).prop_map(LiveCell::Boxed),
            (-1000i64..1000, -1000i64..1000).prop_map(|(a, b)| LiveCell::Pair(a, b)),
        ],
        8..40,
    )
    .prop_map(|cells| WideLiveSpec { cells })
}

fn build_widelive(spec: &WideLiveSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_WIDELIVE);
    let mut b = TreeBuilder::new();

    // Allocate one binder per cell.
    let binders: Vec<VarId> = (0..spec.cells.len()).map(|_| fresh_var()).collect();

    // Body: fold every binding's contained Int#(s) into a sum. We build the body
    // FIRST (innermost), then wrap each LetNonRec around it outermost-last so the
    // root ends up last.
    //
    // Folding extracts the field(s) via a Case on the binder, summing into acc.
    let mut acc = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    for (i, cell) in spec.cells.iter().enumerate() {
        let binder = binders[i];
        let bv = b.push(CoreFrame::Var(binder));
        let case_binder = fresh_var();
        match cell {
            LiveCell::Boxed(_) => {
                let fld = fresh_var();
                let fld_v = b.push(CoreFrame::Var(fld));
                let extracted = b.push(CoreFrame::Case {
                    scrutinee: bv,
                    binder: case_binder,
                    alts: vec![Alt {
                        con: AltCon::DataAlt(I_HASH),
                        binders: vec![fld],
                        body: fld_v,
                    }],
                });
                acc = b.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![acc, extracted],
                });
            }
            LiveCell::Pair(_, _) => {
                let f1 = fresh_var();
                let f2 = fresh_var();
                let f1_v = b.push(CoreFrame::Var(f1));
                let f2_v = b.push(CoreFrame::Var(f2));
                let sum = b.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![f1_v, f2_v],
                });
                let extracted = b.push(CoreFrame::Case {
                    scrutinee: bv,
                    binder: case_binder,
                    alts: vec![Alt {
                        con: AltCon::DataAlt(PAIR),
                        binders: vec![f1, f2],
                        body: sum,
                    }],
                });
                acc = b.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![acc, extracted],
                });
            }
        }
    }

    // Now wrap LetNonRec bindings around `acc`, from the LAST cell inward, so the
    // FIRST cell's let is the outermost (and therefore the final / root node).
    let mut body = acc;
    for i in (0..spec.cells.len()).rev() {
        let rhs = match &spec.cells[i] {
            LiveCell::Boxed(n) => {
                let lit = b.push(CoreFrame::Lit(Literal::LitInt(*n)));
                b.push(CoreFrame::Con {
                    tag: I_HASH,
                    fields: vec![lit],
                })
            }
            LiveCell::Pair(a, c) => {
                let la = b.push(CoreFrame::Lit(Literal::LitInt(*a)));
                let lc = b.push(CoreFrame::Lit(Literal::LitInt(*c)));
                b.push(CoreFrame::Con {
                    tag: PAIR,
                    fields: vec![la, lc],
                })
            }
        };
        body = b.push(CoreFrame::LetNonRec {
            binder: binders[i],
            rhs,
            body,
        });
    }

    let root = body;
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (c) BigCon
//
// One large constructor that approaches/exceeds the nursery in a single object,
// forcing GC during its own construction and verifying every field survives the
// copy. Two shapes:
//   * PairTree: a balanced Pair-tree of depth k (2^k Int# leaves), returned
//     WHOLE, then a checksum walk folds all leaves into a single Int#.
//   * LongSpine: a cons-spine of length `len`, returned WHOLE, then summed.
// Returning the structure WHOLE (not just a scalar) means the entire object is
// the live root through the final GC, then deep-forced by the comparison.
// ===========================================================================

#[derive(Clone, Debug)]
enum BigConSpec {
    PairTree { depth: u32, leaves: Vec<i64> },
    LongSpine { elems: Vec<i64> },
}

fn arb_bigcon() -> impl Strategy<Value = BigConSpec> {
    prop_oneof![
        (2u32..7)
            .prop_flat_map(|depth| {
                let n = 1usize << depth;
                (Just(depth), prop::collection::vec(-1000i64..1000, n..=n))
            })
            .prop_map(|(depth, leaves)| BigConSpec::PairTree { depth, leaves }),
        prop::collection::vec(-1000i64..1000, 4..300)
            .prop_map(|elems| BigConSpec::LongSpine { elems }),
    ]
}

/// Build a balanced Pair-tree of `depth` over `leaves` (must be 2^depth long).
/// Returns the root index of the tree.
fn push_pairtree(b: &mut TreeBuilder, depth: u32, leaves: &[i64]) -> usize {
    if depth == 0 {
        return b.push(CoreFrame::Lit(Literal::LitInt(leaves[0])));
    }
    let half = leaves.len() / 2;
    let l = push_pairtree(b, depth - 1, &leaves[..half]);
    let r = push_pairtree(b, depth - 1, &leaves[half..]);
    b.push(CoreFrame::Con {
        tag: PAIR,
        fields: vec![l, r],
    })
}

/// Build a checksum walk over a balanced Pair-tree of `depth` rooted at `tree`,
/// folding all leaves into a single Int# sum. Unrolled (depth small) so it stays
/// total and ground without recursion.
fn push_pairtree_sum(b: &mut TreeBuilder, depth: u32, tree: usize) -> usize {
    if depth == 0 {
        return tree;
    }
    let case_binder = fresh_var();
    let l = fresh_var();
    let r = fresh_var();
    let l_v = b.push(CoreFrame::Var(l));
    let l_sum = push_pairtree_sum(b, depth - 1, l_v);
    let r_v = b.push(CoreFrame::Var(r));
    let r_sum = push_pairtree_sum(b, depth - 1, r_v);
    let sum = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![l_sum, r_sum],
    });
    b.push(CoreFrame::Case {
        scrutinee: tree,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(PAIR),
            binders: vec![l, r],
            body: sum,
        }],
    })
}

fn build_bigcon(spec: &BigConSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_BIGCON);
    let mut b = TreeBuilder::new();

    match spec {
        BigConSpec::PairTree { depth, leaves } => {
            let tree_root = push_pairtree(&mut b, *depth, leaves);
            let bound = fresh_var();
            let bound_v = b.push(CoreFrame::Var(bound));
            let sum = push_pairtree_sum(&mut b, *depth, bound_v);
            // let big = <pairtree> in <sum walk over big>
            let root = b.push(CoreFrame::LetNonRec {
                binder: bound,
                rhs: tree_root,
                body: sum,
            });
            let mut tree = b.build();
            fixup_root(&mut tree, root)
        }
        BigConSpec::LongSpine { elems } => {
            // let big = <spine> in <tail-rec sum over big>  (LetRec-lambda fold)
            let spine = push_spine(&mut b, elems);
            let bound = fresh_var();
            let fold = push_list_fold(&mut b, bound, 0, Combine::AddHead);
            let root = b.push(CoreFrame::LetNonRec {
                binder: bound,
                rhs: spine,
                body: fold,
            });
            let mut tree = b.build();
            fixup_root(&mut tree, root)
        }
    }
}

// ===========================================================================
// (d) AccumLoop
//
// A join-point counting loop that allocates a fresh boxed Con EVERY iteration
// (immediate garbage) while threading a live accumulator. Targets GC x deep
// recursion: many collection cycles, one long-lived root (the accumulator)
// surrounded by a torrent of dead allocations + a per-iteration live temporary.
//
//   join go (acc, i) =
//     case (i ># LIMIT) of
//       1# -> acc
//       _  -> let box  = I# (acc +# i)        -- live this iteration
//             in let junk = Just (Just (I# i)) -- immediate garbage
//                in case box of I# n -> jump go (n, i +# 1)
//   in jump go (start, 0)
//
// LIMIT up to ~2000 -> thousands of allocations through a 2-4 KiB nursery ->
// dozens of GC cycles, each of which must preserve `acc`/`box` and reclaim the
// junk. A root-tracking bug surfaces as a wrong sum or a crash.
// ===========================================================================

#[derive(Clone, Debug)]
struct AccumLoopSpec {
    limit: i64,
    start: i64,
    /// Extra `Just`-nesting depth on the per-iteration junk (0..3): deeper junk
    /// => larger dead objects => more GC pressure.
    junk_depth: u32,
}

fn arb_accumloop() -> impl Strategy<Value = AccumLoopSpec> {
    (0i64..2000, -100i64..100, 0u32..3).prop_map(|(limit, start, junk_depth)| AccumLoopSpec {
        limit,
        start,
        junk_depth,
    })
}

fn build_accumloop(spec: &AccumLoopSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_ACCUMLOOP);
    let mut b = TreeBuilder::new();

    // Self-recursive LetRec lambda (NOT a join — see push_list_fold rationale):
    //   letrec go = \acc -> \i ->
    //     case (i ># LIMIT) of
    //       1# -> acc
    //       _  -> let box  = I# (acc +# i)
    //             in let junk = Just^depth (I# i)         -- immediate garbage
    //                in case box of I# n -> go n (i +# 1)
    //   in go <start> 0
    let go = fresh_var();
    let acc = fresh_var();
    let i = fresh_var();

    // cond: i ># LIMIT
    let iv = b.push(CoreFrame::Var(i));
    let lim = b.push(CoreFrame::Lit(Literal::LitInt(spec.limit)));
    let cond = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntGt,
        args: vec![iv, lim],
    });

    // done arm: acc
    let done = b.push(CoreFrame::Var(acc));

    // box = I# (acc +# i)
    let av = b.push(CoreFrame::Var(acc));
    let iv2 = b.push(CoreFrame::Var(i));
    let new_val = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![av, iv2],
    });
    let box_rhs = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![new_val],
    });
    let box_binder = fresh_var();

    // junk: Just^depth (I# i) — immediate garbage, never used.
    let iv3 = b.push(CoreFrame::Var(i));
    let mut junk = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![iv3],
    });
    for _ in 0..=spec.junk_depth {
        junk = b.push(CoreFrame::Con {
            tag: JUST,
            fields: vec![junk],
        });
    }
    let junk_binder = fresh_var();

    // case box of I# n -> go n (i +# 1)
    let box_v = b.push(CoreFrame::Var(box_binder));
    let case_binder = fresh_var();
    let n = fresh_var();
    let n_v = b.push(CoreFrame::Var(n));
    let iv4 = b.push(CoreFrame::Var(i));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let new_i = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![iv4, one],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: n_v,
    });
    let recur_call = b.push(CoreFrame::App {
        fun: app1,
        arg: new_i,
    });
    let unbox_case = b.push(CoreFrame::Case {
        scrutinee: box_v,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: recur_call,
        }],
    });
    // let junk = ... in <unbox_case>
    let let_junk = b.push(CoreFrame::LetNonRec {
        binder: junk_binder,
        rhs: junk,
        body: unbox_case,
    });
    // let box = ... in <let_junk>
    let recur = b.push(CoreFrame::LetNonRec {
        binder: box_binder,
        rhs: box_rhs,
        body: let_junk,
    });

    let case_binder2 = fresh_var();
    let body_case = b.push(CoreFrame::Case {
        scrutinee: cond,
        binder: case_binder2,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: done,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: recur,
            },
        ],
    });

    // \acc -> \i -> <body_case>
    let inner_lam = b.push(CoreFrame::Lam {
        binder: i,
        body: body_case,
    });
    let go_lam = b.push(CoreFrame::Lam {
        binder: acc,
        body: inner_lam,
    });

    // body: go <start> 0
    let go_b = b.push(CoreFrame::Var(go));
    let sa = b.push(CoreFrame::Lit(Literal::LitInt(spec.start)));
    let call1 = b.push(CoreFrame::App { fun: go_b, arg: sa });
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: zero,
    });

    let root = b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// Properties.
//
// Two configs per shape: a default-cases run and (via the same generator) the
// small-nursery ladder is ALWAYS applied inside run_gc_oracle, so every case is
// a nursery sweep. We run 400 cases per property (the spec's 300-500 band).
// ===========================================================================

fn cfg() -> Config {
    // Case count is overridable via PROPTEST_CASES (env) for a small "nursery"
    // run vs. the default 400 (the spec's 300-500 band). Failure persistence is
    // OFF: these tests run proptest's runner directly inside a big-stack thread,
    // so the source-file-relative regressions path can't be resolved and only
    // emits a noisy warning.
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(400);
    let mut c = Config::with_cases(cases);
    c.max_shrink_iters = 6000;
    c.failure_persistence = Some(Box::new(proptest::test_runner::FileFailurePersistence::Off));
    c
}

// Each property runs proptest's runner INSIDE a big-stack thread (see
// `with_big_stack`): the generated cons-spines/Pair-trees are deeply-nested host
// trees that overflow the default 2 MiB test stack during eval / Value-drop.

macro_rules! gc_property {
    ($name:ident, $strat:expr, $build:expr) => {
        #[ignore = "heavy GC fuzz (~68min at 400 cases); on-demand: cargo test -p tidepool-codegen --test proptest_gc_recursion -- --ignored"]
        #[test]
        #[serial]
        fn $name() {
            with_big_stack(|| {
                let mut runner = proptest::test_runner::TestRunner::new(cfg());
                runner
                    .run(&$strat, |spec| {
                        let expr = $build(&spec);
                        run_gc_oracle(expr)
                    })
                    .unwrap();
            });
        }
    };
}

gc_property!(prop_cons_spine, arb_consspine(), build_consspine);
gc_property!(prop_wide_live, arb_widelive(), build_widelive);
gc_property!(prop_big_con, arb_bigcon(), build_bigcon);
gc_property!(prop_accum_loop, arb_accumloop(), build_accumloop);

// ===========================================================================
// Deterministic anchor cases: large, fixed allocation shapes at the tiniest
// nursery. These pin specific GC-heavy programs (independent of the random
// seed) so a regression that the random sweep happens to miss still trips.
// ===========================================================================

#[ignore = "heavy GC anchor (tiny-nursery sweep); on-demand: --ignored"]
#[test]
#[serial]
fn anchor_long_spine_sum_tiny_nursery() {
    with_big_stack(anchor_long_spine_sum_tiny_nursery_body);
}
fn anchor_long_spine_sum_tiny_nursery_body() {
    // 500-element spine summed under a 2 KiB nursery: forces many forwards of a
    // single long live chain mid-walk. Sum of 0..499 = 124750.
    let elems: Vec<i64> = (0..500).collect();
    let spec = ConsSpineSpec {
        elems: elems.clone(),
        consumer: SpineConsumer::Sum,
    };
    let expr = build_consspine(&spec);
    let table = build_table_for_expr(&expr);

    let expected: i64 = elems.iter().sum();
    // Eval is the oracle of record.
    use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let ev = eval(&expr, &env, &mut heap).expect("eval should succeed");
    if let tidepool_eval::value::Value::Lit(Literal::LitInt(n)) = &ev {
        assert_eq!(*n, expected, "eval sum mismatch (oracle wrong?)");
    } else {
        panic!("eval produced non-Int result: {:?}", ev);
    }

    for &n in &[64 * 1024usize, 4 * 1024, 2 * 1024] {
        match JitEffectMachine::compile(&expr, &table, n).and_then(|mut m| m.run_pure()) {
            Ok(v) => assert!(
                values_equal(&ev, &v),
                "anchor: JIT (nursery {}) = {:?} != eval {:?}",
                n,
                v,
                ev
            ),
            Err(JitError::Yield(YieldError::Runtime(RuntimeError::HeapOverflow))) => { /* tolerated */
            }
            Err(e) => panic!("anchor: JIT (nursery {}) errored: {:?}", n, e),
        }
    }
}

#[ignore = "heavy GC anchor (1500-iter loop, tiny nursery, >60s); on-demand: --ignored"]
#[test]
#[serial]
fn anchor_accum_loop_tiny_nursery() {
    with_big_stack(anchor_accum_loop_tiny_nursery_body);
}
fn anchor_accum_loop_tiny_nursery_body() {
    // 1500-iteration alloc-per-step loop under a 4 KiB nursery: dozens of GC
    // cycles, one long-lived accumulator. start=0 => sum 0..1500 = 1125750.
    let spec = AccumLoopSpec {
        limit: 1500,
        start: 0,
        junk_depth: 2,
    };
    let expr = build_accumloop(&spec);
    let table = build_table_for_expr(&expr);

    let expected: i64 = (0..=1500).sum();
    use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let ev = eval(&expr, &env, &mut heap).expect("eval should succeed");
    if let tidepool_eval::value::Value::Lit(Literal::LitInt(n)) = &ev {
        assert_eq!(*n, expected, "eval sum mismatch (oracle wrong?)");
    } else {
        panic!("eval produced non-Int result: {:?}", ev);
    }

    for &n in &[64 * 1024usize, 8 * 1024, 4 * 1024] {
        match JitEffectMachine::compile(&expr, &table, n).and_then(|mut m| m.run_pure()) {
            Ok(v) => assert!(
                values_equal(&ev, &v),
                "anchor: JIT (nursery {}) = {:?} != eval {:?}",
                n,
                v,
                ev
            ),
            Err(JitError::Yield(YieldError::Runtime(RuntimeError::HeapOverflow))) => { /* tolerated */
            }
            Err(e) => panic!("anchor: JIT (nursery {}) errored: {:?}", n, e),
        }
    }
}

// ===========================================================================
// Reach report. Ordered last (alphabetical: zzz_ prefix).
// ===========================================================================
#[ignore = "reach report for the --ignored GC fuzz lane (no-op without it)"]
#[test]
#[serial]
fn zzz_reach_report() {
    let total = TOTAL.load(Ordering::Relaxed);
    let reached = REACHED.load(Ordering::Relaxed);
    eprintln!(
        "GC-RECURSION REACH: {}/{} cases reached value comparison ({:.1}%)",
        reached,
        total,
        if total > 0 {
            100.0 * reached as f64 / total as f64
        } else {
            0.0
        }
    );
    eprintln!(
        "SHAPE FREQ: consspine={} widelive={} bigcon={} accumloop={}",
        N_CONSSPINE.load(Ordering::Relaxed),
        N_WIDELIVE.load(Ordering::Relaxed),
        N_BIGCON.load(Ordering::Relaxed),
        N_ACCUMLOOP.load(Ordering::Relaxed),
    );
    eprintln!(
        "GC-COMPLETED at <=4KiB nursery: {} cases",
        N_GC_COMPLETED_TINY.load(Ordering::Relaxed),
    );
    if total >= 100 {
        let ratio = reached as f64 / total as f64;
        assert!(
            ratio >= 0.90,
            "reach floor: only {:.1}% of {} cases reached value comparison (need >= 90%)",
            100.0 * ratio,
            total
        );
    }
}
