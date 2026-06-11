# Proptest findings — freer-simple continuation queue (S2 freer-queue)

Algebraic-law testing of the freer-simple continuation queue (the Leaf/Node
type-aligned tree — locked decision) and the eval-side `EffectMachine`
(`tidepool-effect/src/machine.rs`), differentially against a naive
list-of-functions model. Pure Rust, no JIT, no Haskell.

The `EffectMachine` is the differential ORACLE for every other proptest
wave, so a bug here corrupts other suites' ground truth — that is why it
got its own audit.

Test: `tidepool-effect/tests/proptest_freer_queue.rs`
Seeds: none committed — see "Why no `.proptest-regressions`" below.

## How it works

A *case* = `(leaf sequence, tree shape(s), initial request, response script)`:

* **Leaf alphabet** — simple `Int -> Eff Int` functions built as CoreExpr
  lambdas: `Add(k)` = `\x -> Val (x +# k)`, `Mul(k)`, `Emit(c)` =
  `\x -> E (Union 0 (x +# c)) (Leaf id)` (unconditional effect emission),
  and `EmitIfOdd(c)` = parity-`case` conditional emission (odd → effect,
  even → `Val (x +# 1)`).
* **Tree shapes** — the same in-order leaf sequence is wrapped into:
  seeded random binary shapes (deterministic LCG split function of
  `(leaf count, seed)`, so two seeds ⇒ two in-order-equivalent trees),
  plus fully left-biased and fully right-biased spines built iteratively
  (the test must not recurse where the machine is the component under test).
* **Model** — flat `Vec` of closures applied left-to-right with wrapping
  arithmetic and scripted responses; produces `(final result, dispatch
  transcript of request values)`.
* **Recorder** — a `ScriptHandler` records every dispatched request and
  answers from the script by dispatch index (missing ⇒ 0, same as model).

### Laws tested (all with explicit `ProptestConfig` case counts; 400 total)

| Law | Property | Cases | Verdict |
|-----|----------|-------|---------|
| L1 shape irrelevance (associativity of the type-aligned sequence) | `shape_irrelevance` — two seeded shapes over the same leaf sequence: identical result AND transcript | 140 | GREEN |
| L1 corners + L2 model equivalence | `machine_matches_model` — seeded/left/right shapes vs. naive fold, 1–19 leaves, 0–19 effects | 140 ×3 shapes | GREEN |
| L3 response threading | `response_threading` — pairwise-distinct indexed responses, emit-heavy mix; transcript + result only match the model if response *i* lands at dispatch *i* exactly | 120 | GREEN |
| L4 deep queues | `deep_biased_trees_match_model_64mb_control` — depth-1200 left AND right spines == model (associativity at depth ≥ 1000) | deterministic | GREEN — **but only on a 64MB stack** (B3) |
| L5 qComp (E emitted inside a continuation composes with the pending queue) | exercised by `Emit`/`EmitIfOdd` at random positions in L1–L3 and every 5th leaf of the depth-1200 runs | (above) | GREEN |
| degenerate continuation | `raw_closure_continuation_equals_leaf_wrapped` — bare closure ≡ `Leaf(closure)`, incl. as a `Node` child | deterministic | GREEN |

## Bug table

| ID | Class | Component | Status | Repro |
|----|-------|-----------|--------|-------|
| B3-1 | B3 stack overflow | `machine.rs` `apply_cont`, `Node` arm (recursion into `k1`) | **CONFIRMED** — process abort | `bug_b3_left_biased_depth_1200_8mb_stack` (`#[ignore]`, run with `--ignored`; exit 134/SIGABRT) |
| B3-2 | B3 stack overflow | `machine.rs` `apply_cont`, `Val` step `apply_cont(k2, y)` (source-level tail call, no TCO guarantee in Rust) | **CONFIRMED** — process abort | `bug_b3_right_biased_depth_1200_8mb_stack` (`#[ignore]`; exit 134/SIGABRT) |
| F1 | robustness (missing expected machine error) | `machine.rs` `Val` arms: run loop + `Node` composition | documented, pinned | `finding_f1_zero_field_val_silently_becomes_zero` (green — pins current behavior) |

### B3 — recursive queue walk (the headline)

`apply_cont` walks the Leaf/Node tree by host-stack recursion. Both spines
burn one Rust frame per queue node; measured cost in the dev profile (the
profile every differential suite runs under) is **~10KB per node**:

| Stack | deepest OK | shallowest ABORT |
|-------|-----------:|-----------------:|
| 1.5MB | 100 | 150 |
| 8MB | 700 | 800 |
| 64MB | 5000 | not reached |

Identical thresholds for left- and right-biased spines (the right-spine
"tail call" is not optimized in dev builds; release behavior untested and
irrelevant for the oracle role, which runs under `cargo test`).

Consequences:

* On a default 2MB Rust thread the machine dies somewhere below ~190 queue
  nodes — i.e. a program with a few hundred sequential binds.
* Stack overflow is not unwindable: the failure mode is **process abort**,
  not an `EffectError`, killing the whole test binary. This is exactly the
  host-stack-overflow class from the project's crash history (deep spines →
  silent hang/abort), now confirmed inside the oracle itself.
* **Oracle contamination radius**: any sibling differential suite that
  drives `EffectMachine` with > ~700-node continuation queues on an 8MB
  thread (or > ~100 on small stacks) will abort and can masquerade as a
  JIT-side fault. Sibling suites should cap queue depth or run the oracle
  in an explicitly large-stack thread, as
  `deep_biased_trees_match_model_64mb_control` does.
* Spec deviation note: the task asked for an "8MB control"; the control had
  to be raised to 64MB because 8MB itself aborts between depth 700 and 800
  — the intended control IS the bug.

The fix direction (out of scope here — no fixes per boundary) is an
iterative queue walk: an explicit work-stack of pending `k2`s instead of
recursion, mirroring how `tidepool-eval::eval` already uses an explicit
work stack and how the Stream drain in `run_with_user` already builds list
spines iteratively.

### F1 — zero-field `Val` silently becomes `LitInt(0)`

Both `Val` consumers substitute a default instead of erroring:

* run loop: `fields.first().cloned().unwrap_or(Lit(LitInt(0)))`
* `Node` arm: same pattern when `k1` returns `Val`

Every OTHER constructor (`E`, `Union`, `Leaf`, `Node`) gets a strict
`FieldCountMismatch` check. A malformed zero-field `Val` — exactly the kind
of artifact a codegen bug would produce — therefore evaluates to `0`
instead of surfacing an error. For a component whose job is to be the
ground-truth oracle, leniency here masks the very divergences the
differential suites exist to catch. Pinned by a green test so a future
strictness fix shows up as a deliberate change.

## Tree-shape coverage stats

* **Random shapes**: 140 + 140 + 120 cases; leaf counts 1–23; seeded LCG
  split ⇒ shapes range from fully degenerate spines to balanced; every case
  in `shape_irrelevance` compares two independent shapes over the same
  in-order sequence.
* **Bias mix**: every `machine_matches_model` case additionally runs fully
  left- and fully right-biased spines (280 biased runs at depths up to 19);
  deterministic deep runs at depth 1200 for both biases (green at 64MB),
  depth 64 both biases on a 1.5MB stack (green), and the two ignored
  depth-1200 abort repros.
* **Max depth exercised green**: 1200 (both biases, 64MB). Max depth
  exercised at all: 5000 (probe, 64MB, green); 50_000 was not probed —
  abort threshold scales linearly with stack, ~10KB/node.
* **Alphabet mix**: all four leaf ops uniformly in L1/L2; emit-weighted mix
  in L3; deterministic 5-cycle (2×Add, 2×Mul, 1×EmitIfOdd) in deep runs, so
  qComp composition is exercised ~240 times per deep run with data-dependent
  (parity) emission.

## Why no `.proptest-regressions`

All in-process properties are GREEN — proptest found no shrinkable
counterexample, so it wrote no regression file. The one confirmed bug class
(B3) manifests as un-unwindable process abort, which proptest cannot record
a seed for; it is reproduced instead by deterministic fixed-depth
`#[ignore = "BUG: …"]` tests with measured thresholds documented in their
doc comments.

## Verified negatives (laws that HOLD)

* Associativity (L1): no shape-dependent result or transcript in 140
  random shape pairs + 280 biased-vs-model runs. The `Node` E-composition
  `E(u, Node(k', k2))` preserves in-order leaf sequence under arbitrary
  re-association, including data-dependent emission.
* Response threading (L3): no off-by-one or permutation in dispatch
  indexing in 120 emit-heavy cases with distinct indexed responses.
* qComp (L5): effects emitted from inside continuations compose correctly
  with pending queues at depths up to 1200.
* Degenerate raw-closure continuations are semantically identical to
  `Leaf`-wrapped ones, both at top level and as `Node` children.
* Semantics at depth are CORRECT whenever the stack suffices — B3 is purely
  a stack-discipline bug, not a semantic one.
