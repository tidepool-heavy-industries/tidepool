# tidepool-eval — tree-walking interpreter (the JIT's oracle)

A lazy, big-step evaluator over `CoreExpr`. Its sole reason to exist is
differential testing: it must agree with `tidepool-codegen`'s Cranelift JIT on
every observable result, so JIT bugs surface as eval-vs-JIT mismatches instead
of silent wrong answers. See the repo-root `CLAUDE.md` for the project map and
locked decisions (CoreFrame variants, CBOR, etc. — not repeated here).

## Reading path

`eval.rs` module doc names the order: `eval` (entry point, builds the datacon
env) → `eval_settled` (the trampoline) → `force` (WHNF reduction) → the big
`match op { … }` primop section, one arm per `PrimOpKind`, **kept in lockstep
with `tidepool-codegen/src/emit/primop.rs` and `define_primops!` in
`tidepool-repr/src/types.rs`** — adding a primop means touching all three.

## Non-obvious design points

- **Join points evaluate via a trampoline, not recursion.** `eval_at` never
  recurses on a `Jump` — join points are GHC's non-stack-growing goto, and
  recursing here would defeat that. It captures a `JumpReq`, unwinds to the
  nearest driver, and `eval_settled` loops. Host-stack use is O(1) in the
  number of self-jumps, mirroring the JIT's TCO. Don't "simplify" this into
  direct recursion — it reintroduces stack growth the JIT doesn't have,
  breaking the oracle comparison on deep loops.
- **`Value` is WHNF-only**; laziness lives in `Heap`'s `ThunkState`
  (`Unevaluated(Env, CoreExpr) → BlackHole → Evaluated(Value)`), the standard
  GHC thunk lifecycle. `BlackHole` is the circular-dependency (`<<loop>>`)
  detector.
- **`Env` is `im::HashMap<VarId, Value>`** (structural sharing) — closures
  clone the whole env cheaply. Don't swap this for `std::HashMap` without
  checking closure-capture costs.
- **`SharedByteArray = Arc<Mutex<Vec<u8>>>`** (byte arrays / mutable arrays).
  Load-bearing invariant, stated on the type: never hold two locks on two
  different `SharedByteArray`s within one primop — clone data out first, or
  it deadlocks.
- **`Heap` is a trait**, not just `VecHeap` — the interpreter is written
  against `&mut dyn Heap` throughout, decoupling it from memory strategy on
  purpose, even though `VecHeap` is currently the only implementation.
- **`Pass` (`pass.rs`) is declared here but not exercised in this crate** —
  its only in-crate impl is `#[cfg(test)] NoOpPass`, with zero non-test call
  sites. The real implementations (`BetaReduce`/`Dce`/`Inline`/`CaseReduce`/
  `PartialEval`) live in `tidepool-optimize`, not here. If you're looking for
  where a `Pass` actually runs, look there.

## Differential testing — how this crate is actually exercised

Nobody unit-tests `tidepool-eval` in isolation for correctness; it's checked
by agreeing with the JIT. The differential harnesses live in
`tidepool-codegen/tests/` and `tidepool-runtime/tests/`:
`proptest_jit_vs_eval.rs`, `proptest_primops_differential.rs`,
`haskell_suite_differential.rs`, `joinrec_differential.rs`,
`normalize_differential.rs`, `sized_addr_primop_differential.rs`,
`primop_bitcount_differential.rs`, and the real-world corpus replay
`real_core_corpus.rs` (replays actual `-O2` Core through both paths). **Any
eval.rs change should be checked against these, not just `cargo test -p
tidepool-eval`** — a primop added to eval but not codegen (or vice versa)
won't fail eval's own test suite, only the differential ones.
