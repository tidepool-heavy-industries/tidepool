//! Algebraic-law property tests for the freer-simple continuation queue
//! (Leaf/Node type-aligned tree) and the eval-side `EffectMachine`.
//!
//! Strategy: random continuation TREES over a fixed alphabet of simple
//! `Int -> Eff Int` leaf functions (+k, *k, unconditional effect emission,
//! parity-conditional effect emission), differentially tested against a
//! naive MODEL: the same computation as a flat in-order `Vec` of closures
//! applied left-to-right with scripted effect responses.
//!
//! Laws under test:
//!   L1 shape irrelevance — any two trees with the same in-order leaf
//!      sequence produce identical results AND identical dispatch
//!      transcripts (associativity of the type-aligned sequence).
//!   L2 model equivalence — machine result == naive fold.
//!   L3 response threading — scripted response i lands at dispatch index i
//!      exactly (transcript comparison with distinct indexed responses).
//!   L4 deep queues — 1000+-node left- and right-biased trees equal the
//!      model without stack overflow (queue walk must be iterative).
//!   L5 qComp — an E emitted from inside a continuation composes correctly
//!      with the pending queue (covered by Emit/EmitIfOdd leaves at random
//!      positions, plus the deep biased runs which sprinkle emits).
//!
//! Findings from this suite are catalogued in
//! `plans/proptest-findings-freer-queue.md`.

use frunk::hlist;
use proptest::prelude::*;
use tidepool_effect::dispatch::{EffectContext, EffectHandler, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::machine::EffectMachine;
use tidepool_eval::heap::VecHeap;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree};

// ---------------------------------------------------------------------------
// Table (mirrors proptest_effect_machine.rs — integration tests are separate
// crates, so the table constructor cannot be shared without a common module).
// ---------------------------------------------------------------------------

const VAL: DataConId = DataConId(1);
const E: DataConId = DataConId(2);
const LEAF: DataConId = DataConId(3);
const NODE: DataConId = DataConId(4);
const UNION: DataConId = DataConId(5);

fn make_test_table() -> DataConTable {
    let mut table = DataConTable::new();
    let mut ins = |id: DataConId, name: &str, tag: u32, rep_arity: u32| {
        table.insert(DataCon {
            id,
            name: name.into(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: None,
        });
    };
    ins(VAL, "Val", 1, 1);
    ins(E, "E", 2, 2);
    ins(LEAF, "Leaf", 1, 1);
    ins(NODE, "Node", 2, 2);
    ins(UNION, "Union", 1, 2);
    table
}

// ---------------------------------------------------------------------------
// Leaf alphabet
// ---------------------------------------------------------------------------

/// One leaf of the continuation tree: a simple `Int -> Eff Int` function.
#[derive(Clone, Debug, PartialEq)]
enum LeafOp {
    /// `\x -> Val (x +# k)`
    Add(i64),
    /// `\x -> Val (x *# k)`
    Mul(i64),
    /// `\x -> E (Union 0 (x +# c)) (Leaf (\y -> Val y))` — always emits.
    Emit(i64),
    /// `\x -> case x remInt# 2 of { 0 -> Val (x +# 1); _ -> emit (x +# c) }`
    /// — conditional emission (odd → effect, even → pure).
    EmitIfOdd(i64),
}

fn leaf_op_strategy() -> impl Strategy<Value = LeafOp> {
    prop_oneof![
        (-1_000i64..1_000).prop_map(LeafOp::Add),
        (-9i64..9).prop_map(LeafOp::Mul),
        (-1_000i64..1_000).prop_map(LeafOp::Emit),
        (-1_000i64..1_000).prop_map(LeafOp::EmitIfOdd),
    ]
}

// ---------------------------------------------------------------------------
// Naive model: flat list of closures applied left-to-right
// ---------------------------------------------------------------------------

/// Run the in-order leaf sequence against scripted responses.
/// Returns (final result, dispatch transcript of request values).
/// Dispatch 0 is the program's initial effect (request = `init_req`);
/// responses index the dispatch sequence; missing responses default to 0
/// (the recorder handler uses the same default).
fn model_run(leaves: &[LeafOp], init_req: i64, responses: &[i64]) -> (i64, Vec<i64>) {
    let mut transcript = vec![init_req];
    let mut next_resp = 1usize;
    let mut acc = responses.first().copied().unwrap_or(0);
    let emit = |acc: &mut i64, c: i64, transcript: &mut Vec<i64>, next_resp: &mut usize| {
        let req = acc.wrapping_add(c);
        transcript.push(req);
        *acc = responses.get(*next_resp).copied().unwrap_or(0);
        *next_resp += 1;
    };
    for leaf in leaves {
        match *leaf {
            LeafOp::Add(k) => acc = acc.wrapping_add(k),
            LeafOp::Mul(k) => acc = acc.wrapping_mul(k),
            LeafOp::Emit(c) => emit(&mut acc, c, &mut transcript, &mut next_resp),
            LeafOp::EmitIfOdd(c) => {
                if acc.wrapping_rem(2) == 0 {
                    acc = acc.wrapping_add(1);
                } else {
                    emit(&mut acc, c, &mut transcript, &mut next_resp);
                }
            }
        }
    }
    (acc, transcript)
}

// ---------------------------------------------------------------------------
// Transcript-recording scripted handler
// ---------------------------------------------------------------------------

struct ScriptHandler {
    responses: Vec<i64>,
    requests: Vec<i64>,
}

impl ScriptHandler {
    fn new(responses: Vec<i64>) -> Self {
        Self {
            responses,
            requests: Vec::new(),
        }
    }
}

impl EffectHandler<()> for ScriptHandler {
    type Request = Value;
    fn handle(
        &mut self,
        req: Self::Request,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        let n = match req {
            Value::Lit(Literal::LitInt(n)) => n,
            other => {
                return Err(EffectError::UnexpectedValue {
                    context: "LitInt request",
                    got: format!("{other:?}"),
                })
            }
        };
        let idx = self.requests.len();
        self.requests.push(n);
        let resp = self.responses.get(idx).copied().unwrap_or(0);
        Ok(Value::Lit(Literal::LitInt(resp)).into())
    }
}

// ---------------------------------------------------------------------------
// CoreExpr builder
// ---------------------------------------------------------------------------

struct Builder {
    nodes: Vec<CoreFrame<usize>>,
    next_var: u64,
}

impl Builder {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            // Leaves a wide margin below any VarId used elsewhere.
            next_var: 10_000,
        }
    }

    fn push(&mut self, f: CoreFrame<usize>) -> usize {
        self.nodes.push(f);
        self.nodes.len() - 1
    }

    fn fresh(&mut self) -> VarId {
        self.next_var += 1;
        VarId(self.next_var)
    }

    fn lit_int(&mut self, n: i64) -> usize {
        self.push(CoreFrame::Lit(Literal::LitInt(n)))
    }

    fn con(&mut self, tag: DataConId, fields: Vec<usize>) -> usize {
        self.push(CoreFrame::Con { tag, fields })
    }

    /// `E (Union 0 <req_idx>) <k_idx>` — k may be any continuation index.
    fn effect(&mut self, req_idx: usize, k_idx: usize) -> usize {
        let tag = self.push(CoreFrame::Lit(Literal::LitWord(0)));
        let union = self.con(UNION, vec![tag, req_idx]);
        self.con(E, vec![union, k_idx])
    }

    /// The lambda implementing one leaf function (UNWRAPPED — no Leaf con).
    fn leaf_lam(&mut self, op: &LeafOp) -> usize {
        match *op {
            LeafOp::Add(k) => self.arith_lam(PrimOpKind::IntAdd, k),
            LeafOp::Mul(k) => self.arith_lam(PrimOpKind::IntMul, k),
            LeafOp::Emit(c) => {
                let vx = self.fresh();
                let id_leaf = self.identity_leaf();
                let x = self.push(CoreFrame::Var(vx));
                let cl = self.lit_int(c);
                let req = self.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![x, cl],
                });
                let e = self.effect(req, id_leaf);
                self.push(CoreFrame::Lam {
                    binder: vx,
                    body: e,
                })
            }
            LeafOp::EmitIfOdd(c) => {
                let vx = self.fresh();
                // even branch: Val (x +# 1)
                let x1 = self.push(CoreFrame::Var(vx));
                let one = self.lit_int(1);
                let xp1 = self.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![x1, one],
                });
                let even_body = self.con(VAL, vec![xp1]);
                // odd branch: E (Union 0 (x +# c)) (Leaf id)
                let id_leaf = self.identity_leaf();
                let x2 = self.push(CoreFrame::Var(vx));
                let cl = self.lit_int(c);
                let req = self.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntAdd,
                    args: vec![x2, cl],
                });
                let odd_body = self.effect(req, id_leaf);
                // case (x remInt# 2) of { 0 -> even; _ -> odd }
                let x3 = self.push(CoreFrame::Var(vx));
                let two = self.lit_int(2);
                let rem = self.push(CoreFrame::PrimOp {
                    op: PrimOpKind::IntRem,
                    args: vec![x3, two],
                });
                let vb = self.fresh();
                let case = self.push(CoreFrame::Case {
                    scrutinee: rem,
                    binder: vb,
                    alts: vec![
                        Alt {
                            con: AltCon::LitAlt(Literal::LitInt(0)),
                            binders: vec![],
                            body: even_body,
                        },
                        Alt {
                            con: AltCon::Default,
                            binders: vec![],
                            body: odd_body,
                        },
                    ],
                });
                self.push(CoreFrame::Lam {
                    binder: vx,
                    body: case,
                })
            }
        }
    }

    /// `\x -> Val (x <op># k)`
    fn arith_lam(&mut self, op: PrimOpKind, k: i64) -> usize {
        let vx = self.fresh();
        let x = self.push(CoreFrame::Var(vx));
        let kl = self.lit_int(k);
        let r = self.push(CoreFrame::PrimOp {
            op,
            args: vec![x, kl],
        });
        let val = self.con(VAL, vec![r]);
        self.push(CoreFrame::Lam {
            binder: vx,
            body: val,
        })
    }

    /// `Leaf (\y -> Val y)`
    fn identity_leaf(&mut self) -> usize {
        let vy = self.fresh();
        let y = self.push(CoreFrame::Var(vy));
        let val = self.con(VAL, vec![y]);
        let lam = self.push(CoreFrame::Lam {
            binder: vy,
            body: val,
        });
        self.con(LEAF, vec![lam])
    }

    /// `Leaf <leaf-lam>`
    fn leaf_con(&mut self, op: &LeafOp) -> usize {
        let lam = self.leaf_lam(op);
        self.con(LEAF, vec![lam])
    }
}

// ---------------------------------------------------------------------------
// Tree shapes
// ---------------------------------------------------------------------------

/// Deterministic split-mix style generator so tree SHAPE is a pure function
/// of (leaf count, seed): two seeds over the same leaf sequence give two
/// in-order-equivalent trees.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Random-shape tree over `leaves` (in order). Recursion depth <= leaf count,
/// which proptest bounds to small values — test-side recursion is safe here.
fn build_random_tree(b: &mut Builder, leaves: &[LeafOp], rng: &mut Lcg) -> usize {
    if leaves.len() == 1 {
        return b.leaf_con(&leaves[0]);
    }
    let split = 1 + (rng.next() as usize % (leaves.len() - 1));
    let left = build_random_tree(b, &leaves[..split], rng);
    let right = build_random_tree(b, &leaves[split..], rng);
    b.con(NODE, vec![left, right])
}

/// `Node(Node(Node(L0, L1), L2), ...)` — depth = n-1 on the LEFT spine.
/// Built iteratively: the test must not recurse where the machine is the
/// component under test.
fn build_left_biased(b: &mut Builder, leaves: &[LeafOp]) -> usize {
    let mut acc = b.leaf_con(&leaves[0]);
    for op in &leaves[1..] {
        let l = b.leaf_con(op);
        acc = b.con(NODE, vec![acc, l]);
    }
    acc
}

/// `Node(L0, Node(L1, Node(L2, ...)))` — depth = n-1 on the RIGHT spine.
fn build_right_biased(b: &mut Builder, leaves: &[LeafOp]) -> usize {
    let mut acc = b.leaf_con(&leaves[leaves.len() - 1]);
    for op in leaves[..leaves.len() - 1].iter().rev() {
        let l = b.leaf_con(op);
        acc = b.con(NODE, vec![l, acc]);
    }
    acc
}

#[derive(Clone, Copy, Debug)]
enum TreeShape {
    Seeded(u64),
    LeftBiased,
    RightBiased,
}

/// Top-level program: `E (Union 0 init_req) <tree>`.
fn build_program(leaves: &[LeafOp], shape: TreeShape, init_req: i64) -> CoreExpr {
    let mut b = Builder::new();
    let tree = match shape {
        TreeShape::Seeded(seed) => {
            let mut rng = Lcg(seed);
            build_random_tree(&mut b, leaves, &mut rng)
        }
        TreeShape::LeftBiased => build_left_biased(&mut b, leaves),
        TreeShape::RightBiased => build_right_biased(&mut b, leaves),
    };
    let init = b.lit_int(init_req);
    b.effect(init, tree);
    RecursiveTree { nodes: b.nodes }
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Run a program against a fresh machine + recorder; return
/// (final result, dispatch transcript).
fn run_program(expr: &CoreExpr, responses: Vec<i64>) -> Result<(i64, Vec<i64>), String> {
    let table = make_test_table();
    let mut heap = VecHeap::new();
    let mut handlers = hlist![ScriptHandler::new(responses)];
    let mut machine = EffectMachine::new(&table, &mut heap).map_err(|e| format!("{e:?}"))?;
    let result = machine
        .run(expr, &mut handlers)
        .map_err(|e| format!("{e:?}"))?;
    let n = match result {
        Value::Lit(Literal::LitInt(n)) => n,
        other => return Err(format!("non-LitInt result: {other:?}")),
    };
    Ok((n, handlers.head.requests))
}

fn run_in_thread<T: Send + 'static>(
    stack_bytes: usize,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    std::thread::Builder::new()
        .stack_size(stack_bytes)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("thread panicked")
}

/// Deterministic leaf mix for the deep biased runs: arithmetic with
/// parity-conditional emits sprinkled every 5th leaf (so qComp composition
/// is exercised under depth, not just pure Val threading).
fn deep_leaf_mix(n: usize) -> Vec<LeafOp> {
    (0..n)
        .map(|i| match i % 5 {
            0 => LeafOp::Add(i as i64),
            1 => LeafOp::Mul(3),
            2 => LeafOp::Add(-(i as i64) - 7),
            3 => LeafOp::EmitIfOdd(11),
            _ => LeafOp::Mul(-1),
        })
        .collect()
}

fn indexed_responses(n: usize) -> Vec<i64> {
    (0..n as i64)
        .map(|i| (i + 1).wrapping_mul(1_000_003))
        .collect()
}

// ---------------------------------------------------------------------------
// L1 shape irrelevance + L2 model equivalence + L3 response threading
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(140))]

    /// L1: two trees with the same in-order leaf sequence are
    /// indistinguishable — same result, same dispatch transcript.
    #[test]
    fn shape_irrelevance(
        leaves in prop::collection::vec(leaf_op_strategy(), 1..24),
        seed_a in any::<u64>(),
        seed_b in any::<u64>(),
        init in any::<i64>(),
        responses in prop::collection::vec(any::<i64>(), 0..32),
    ) {
        let prog_a = build_program(&leaves, TreeShape::Seeded(seed_a), init);
        let prog_b = build_program(&leaves, TreeShape::Seeded(seed_b), init);
        let (ra, ta) = run_program(&prog_a, responses.clone()).unwrap();
        let (rb, tb) = run_program(&prog_b, responses).unwrap();
        prop_assert_eq!(ra, rb, "shape-dependent RESULT (associativity violation)");
        prop_assert_eq!(ta, tb, "shape-dependent TRANSCRIPT (associativity violation)");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(140))]

    /// L1 (biased corners) + L2: left-biased, right-biased, and seeded trees
    /// all equal the naive flat-fold model.
    #[test]
    fn machine_matches_model(
        leaves in prop::collection::vec(leaf_op_strategy(), 1..20),
        seed in any::<u64>(),
        init in any::<i64>(),
        responses in prop::collection::vec(any::<i64>(), 0..24),
    ) {
        let (mr, mt) = model_run(&leaves, init, &responses);
        for shape in [TreeShape::Seeded(seed), TreeShape::LeftBiased, TreeShape::RightBiased] {
            let prog = build_program(&leaves, shape, init);
            let (r, t) = run_program(&prog, responses.clone()).unwrap();
            prop_assert_eq!(r, mr, "result diverges from model for {:?}", shape);
            prop_assert_eq!(t.clone(), mt.clone(), "transcript diverges from model for {:?}", shape);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// L3: with pairwise-distinct indexed responses and an emit-heavy leaf
    /// mix, the final result + transcript only match the model if response i
    /// landed at dispatch index i exactly (any permutation or off-by-one
    /// changes the downstream request chain).
    #[test]
    fn response_threading(
        emits in prop::collection::vec(
            prop_oneof![
                (-500i64..500).prop_map(LeafOp::Emit),
                (-500i64..500).prop_map(LeafOp::EmitIfOdd),
                (-50i64..50).prop_map(LeafOp::Add),
            ],
            1..16,
        ),
        seed in any::<u64>(),
        init in any::<i64>(),
        jitter in 0i64..1000,
    ) {
        let responses: Vec<i64> = indexed_responses(20)
            .into_iter()
            .map(|r| r.wrapping_add(jitter))
            .collect();
        let (mr, mt) = model_run(&emits, init, &responses);
        let prog = build_program(&emits, TreeShape::Seeded(seed), init);
        let (r, t) = run_program(&prog, responses).unwrap();
        prop_assert_eq!(t.len(), mt.len(), "dispatch COUNT diverges");
        prop_assert_eq!(t, mt, "request transcript diverges — response misthreaded");
        prop_assert_eq!(r, mr);
    }
}

// ---------------------------------------------------------------------------
// L4 deep queues (8MB control — green) + L5 qComp under depth
// ---------------------------------------------------------------------------

/// Depth-1200 left-biased vs right-biased trees over the same leaf sequence:
/// associativity at depth >= 1000, model equality, and qComp composition
/// (every 5th leaf conditionally emits).
///
/// NOTE the stack size: 64MB, not the originally intended 8MB control.
/// Measured (dev profile): the recursive `apply_cont` burns ~10KB of host
/// stack PER QUEUE NODE, so an 8MB thread aborts between depth 700 and 800
/// for BOTH biases. 64MB comfortably runs depth 5000. The semantics at
/// depth 1200 are correct (this test is green) — only the stack discipline
/// is broken, which is bug B3 below.
#[test]
fn deep_biased_trees_match_model_64mb_control() {
    run_in_thread(64 * 1024 * 1024, || {
        let n = 1200;
        let leaves = deep_leaf_mix(n);
        let responses = indexed_responses(n / 4 + 2);
        let (mr, mt) = model_run(&leaves, 17, &responses);
        for shape in [TreeShape::LeftBiased, TreeShape::RightBiased] {
            let prog = build_program(&leaves, shape, 17);
            let (r, t) = run_program(&prog, responses.clone()).unwrap();
            assert_eq!(r, mr, "deep {shape:?} result diverges from model");
            assert_eq!(t, mt, "deep {shape:?} transcript diverges from model");
        }
    });
}

/// Boundary evidence for B3: a depth the recursive queue walk can still
/// survive on a deliberately small (1.5MB) stack. Establishes that the
/// small-stack harness itself is sound — the ignored B3 repros below fail
/// by DEPTH, not by harness construction. Measured boundary on 1.5MB (dev
/// profile): depth 100 OK, depth 150 ABORTS.
#[test]
fn small_stack_depth_64_green() {
    run_in_thread(1_536 * 1024, || {
        let n = 64;
        let leaves = deep_leaf_mix(n);
        let responses = indexed_responses(n / 4 + 2);
        let (mr, mt) = model_run(&leaves, 17, &responses);
        for shape in [TreeShape::LeftBiased, TreeShape::RightBiased] {
            let prog = build_program(&leaves, shape, 17);
            let (r, t) = run_program(&prog, responses.clone()).unwrap();
            assert_eq!(r, mr);
            assert_eq!(t, mt);
        }
    });
}

/// BUG B3 (left spine): `EffectMachine::apply_cont` recurses into `k1` for
/// every `Node` (machine.rs Node arm), so a left-biased tree consumes one
/// host stack frame (~10KB in the dev profile) per node. Depth 1200 on an
/// 8MB stack — four times the Rust spawned-thread default — overflows and
/// ABORTS the process (stack overflow is not unwindable), which is why this
/// repro must stay ignored rather than be a normal failing test. Measured
/// thresholds (8MB, dev): depth 700 OK, depth 800 ABORTS; on a default 2MB
/// thread the machine dies somewhere below ~190 nodes.
///
/// observed: process abort — "thread ... has overflowed its stack"
/// expected: result == model (queue walk must be iterative)
/// class: B3 stack overflow | component: tidepool-effect/src/machine.rs apply_cont
/// seed: deterministic (no proptest seed — fixed-depth repro)
// B3 PARTIALLY FIXED 2026-06-11: apply_cont's queue walk is now iterative
// (explicit pending stack) — RUNTIME queue depth (e.g. mapM over a long
// effect list, where the expression stays small) no longer consumes host
// stack. This repro however builds a 1200-deep EXPRESSION, and post-fix
// probing shows the abort here is dominated by eval_at's expression
// recursion (gotcha #5: ~600 OK / 700 ABORT at 8MB dev) — a different
// walker, planned as its own slice. Activate when the eval slice lands.
#[test]
#[ignore = "blocked on eval_at expression recursion (gotcha #5): repro depth is expression depth, not queue depth; apply_cont itself is iterative since 2026-06-11"]
fn bug_b3_left_biased_depth_1200_8mb_stack() {
    run_in_thread(8 * 1024 * 1024, || {
        let n = 1_200;
        let leaves: Vec<LeafOp> = (0..n).map(|i| LeafOp::Add(i as i64)).collect();
        let (mr, _) = model_run(&leaves, 1, &[5]);
        let prog = build_program(&leaves, TreeShape::LeftBiased, 1);
        let (r, _) = run_program(&prog, vec![5]).unwrap();
        assert_eq!(r, mr);
    });
}

/// BUG B3 (right spine): the `Val` continuation step `apply_cont(k2, y)` is
/// a tail call in source but Rust guarantees no TCO; in the dev profile each
/// right-spine step also burns a host frame, so right-biased trees abort at
/// the same measured thresholds as left-biased ones (8MB: 700 OK / 800 ABORT).
///
/// observed: process abort — "thread ... has overflowed its stack"
/// expected: result == model
/// class: B3 stack overflow | component: tidepool-effect/src/machine.rs apply_cont
/// seed: deterministic (fixed-depth repro)
// Same status and caveat as the left-biased twin: queue walk iterative,
// repro blocked on eval_at expression recursion.
#[test]
#[ignore = "blocked on eval_at expression recursion (gotcha #5): repro depth is expression depth, not queue depth; apply_cont itself is iterative since 2026-06-11"]
fn bug_b3_right_biased_depth_1200_8mb_stack() {
    run_in_thread(8 * 1024 * 1024, || {
        let n = 1_200;
        let leaves: Vec<LeafOp> = (0..n).map(|i| LeafOp::Add(i as i64)).collect();
        let (mr, _) = model_run(&leaves, 1, &[5]);
        let prog = build_program(&leaves, TreeShape::RightBiased, 1);
        let (r, _) = run_program(&prog, vec![5]).unwrap();
        assert_eq!(r, mr);
    });
}

// ---------------------------------------------------------------------------
// Robustness probes (documented behavior, candidate findings)
// ---------------------------------------------------------------------------

/// The machine accepts a RAW closure (not wrapped in `Leaf`) as a
/// continuation — the documented "degenerate continuation" arm. Law: a raw
/// closure must be indistinguishable from `Leaf(closure)`, both at the top
/// level and as a `Node` child.
#[test]
fn raw_closure_continuation_equals_leaf_wrapped() {
    let leaves = vec![LeafOp::Add(40), LeafOp::Emit(3), LeafOp::Mul(7)];
    let responses = vec![2, 50];
    // Reference: all leaves Leaf-wrapped, right-biased.
    let reference = build_program(&leaves, TreeShape::RightBiased, 9);
    let (r_ref, t_ref) = run_program(&reference, responses.clone()).unwrap();

    // Variant: same computation but the first leaf is a BARE lambda used
    // directly as k1 of a Node, and the top-level k of the last emit's
    // continuation chain stays Leaf-wrapped.
    let mut b = Builder::new();
    let bare = b.leaf_lam(&leaves[0]); // raw closure, no Leaf con
    let mid = b.leaf_con(&leaves[1]);
    let last = b.leaf_con(&leaves[2]);
    let inner = b.con(NODE, vec![mid, last]);
    let tree = b.con(NODE, vec![bare, inner]);
    let init = b.lit_int(9);
    b.effect(init, tree);
    let variant = RecursiveTree { nodes: b.nodes };
    let (r_var, t_var) = run_program(&variant, responses).unwrap();

    assert_eq!(
        r_var, r_ref,
        "raw-closure continuation diverges from Leaf-wrapped"
    );
    assert_eq!(t_var, t_ref);
}

/// FINDING F1 (robustness, candidate B2-inverse): a malformed `Val` with
/// ZERO fields is silently accepted — both the top-level run loop and the
/// Node composition arm substitute `LitInt(0)` via `.first().unwrap_or(...)`
/// instead of reporting `FieldCountMismatch` (machine.rs Val arms). Compare:
/// every OTHER constructor arity is strictly checked. This masks malformed
/// trees produced by codegen bugs — exactly what the machine's role as
/// differential oracle is supposed to surface.
///
/// F1 FIXED 2026-06-11: both Val arms now enforce arity 1 and report
/// FieldCountMismatch instead of fabricating LitInt(0). This is the active
/// regression test for the strict contract.
#[test]
fn finding_f1_zero_field_val_silently_becomes_zero() {
    // Top level: program is `Val` with no fields -> clean error.
    let mut b = Builder::new();
    b.con(VAL, vec![]);
    let prog = RecursiveTree { nodes: b.nodes };
    let err = run_program(&prog, vec![]).expect_err("zero-field Val must be rejected");
    assert!(
        format!("{err:?}").contains("Val"),
        "error should name the Val constructor: {err:?}"
    );

    // Node arm: a leaf returning zero-field Val feeds k2 a synthesized 0.
    let mut b = Builder::new();
    let vx = b.fresh();
    let bad_val = b.con(VAL, vec![]);
    let bad_lam = b.push(CoreFrame::Lam {
        binder: vx,
        body: bad_val,
    });
    let bad_leaf = b.con(LEAF, vec![bad_lam]);
    let add = b.leaf_con(&LeafOp::Add(5));
    let tree = b.con(NODE, vec![bad_leaf, add]);
    let init = b.lit_int(1);
    b.effect(init, tree);
    let prog = RecursiveTree { nodes: b.nodes };
    let err = run_program(&prog, vec![100]).expect_err("Node arm must reject zero-field Val");
    assert!(
        format!("{err:?}").contains("Val"),
        "error should name the Val constructor: {err:?}"
    );
}
