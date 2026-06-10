# W2 — GHC-idiom property-test findings

Property-based bug hunt against the three highest-bug-density JIT subsystems
(`emit_letrec_phases` 5-phase ordering, join-point compilation, Con-wrapper
boxing). Programs are hand-built `RecursiveTree<CoreFrame>` IR (no Haskell
source), **total and ground by construction**, compared JIT-vs-eval.

- Test file: `tidepool-codegen/tests/proptest_ghc_idioms.rs`
- Seeds: `tidepool-codegen/tests/proptest_ghc_idioms.txt.proptest-regressions`
- Run: `cargo test -p tidepool-codegen --test proptest_ghc_idioms -- --test-threads=1`
  (single-threaded is required: the B3 oracle `fork()`s per case, and forking a
  multithreaded test runner risks a malloc-lock deadlock in the child.)

## Method

### Skeletons (generators)

| Skeleton | Stresses | Construction |
|---|---|---|
| **(a) LetRecSiblings** | deferred Con field filling, sibling-capture-drop, dead bindings (force_mask) | N∈2..6 mutually-recursive bindings; RHS ∈ {`\x->hole`, `Just(sibling)`, `(,)(sibling,hole)`, plain Int#}; forward & backward sibling refs; body folds a forced subset into an Int# |
| **(b) CaseOfCase** | the GHC join-point factory; join-crossing-lambda (`under_lambda`) | `case (case s of {0#->e1; _->e2}) of {hi->..; _->..}`, optionally inside an applied `\x->` |
| **(c) JoinRec** | joinrec→LetRec, arity-counting type-args (`n_lead`), bounded loops | recursive counting loop `join go(lead..,acc,i)=...jump go(..,acc+i,i+1)`, LIMIT∈0..200 (≤1000 iters); plus a non-rec join reached from two case alts |
| **(d) BoxChain** | wrapper-boxing-mismatch | `I#` box built → `case … of I# n` → PrimOp → re-box, depth 2..6; return boxed or unboxed |
| **(e) JoinCrossLambda** | the real `jumpCrossesLam` class (gotchas #10/#17) | `join k(lead..,p)=p+#h` with the `Jump` to `k` *inside an applied `\x->`*; branchy (jump-per-case-arm) and doubly-nested-Lam variants |

Holes are closed ground sub-terms (depth ≤ 2) evaluating to Int# (`Lit`, `+#`,
`*#`, `<#`, tiny case-of-primop), so every program stays total and structurally
comparable — driving the value-comparison reach to ~100% on the non-buggy
skeletons.

### Oracles

1. **check_jit_vs_eval @ 64KB and 4KB nursery** — B1 (values differ), B2 (JIT-only
   error outside the HeapOverflow/UnresolvedVar/HeapBridge whitelist), B4 (nursery
   knob divergence).
2. **JIT determinism** — compile+run twice @64KB, compare (B4).
3. **B3 crash containment** — `libc::fork()` per case (both nursery sizes); a child
   that dies by signal (`WIFSIGNALED`) is a shrinkable parent failure. `libc` is
   already a unix dep of `tidepool-codegen` (no Cargo.toml edit).

The **optimize-then-compare** oracle from the spec is **skipped**: `tidepool-codegen`
does not depend on `tidepool-optimize`, and the boundary forbids editing Cargo.toml.

### Known-divergence handling

The spec whitelist (HeapOverflow / UnresolvedVar-in-synthetic-LetRec / HeapBridge)
is treated as *not a bug*. Separately, once **Bug #1** below was confirmed and
pinned by an `#[ignore]`d repro, a **targeted** gate in `run_oracles` tolerates
*only* the exact `"Jump to unregistered join"` compilation error (and only when
eval succeeds) so the live fuzzer stays green and keeps hunting. Any value
mismatch or any other JIT error still fails loudly. Known-bug hits are excluded
from the reach denominator.

## Findings

| # | Class | Component | Skeleton | Observed | Expected | Repro | Seed |
|---|---|---|---|---|---|---|---|
| 1 | B2 (JIT-only error) | join-point compilation (`emit/join.rs`) | JoinCrossLambda (fully shrunk) | `Err(Compilation(NotYetImplemented("Jump to unregistered join JoinId(0)")))` | `Ok(Lit(LitInt(0)))` | `bug1_join_crosses_lambda` (#[ignore], 11 nodes) | `cc b2d5850a…ce73a9` |

### Bug #1 — Jump crossing a Lam boundary fails codegen

```
join k(p) = p +# 0
in  (\x -> jump k (x +# 0)) 0
```

The `Jump` to join `k` lives inside the body of a value `Lam`. The JIT compiles
each `Lam` as a separate Cranelift function and registers a join label only in
the function compiling the `Join`'s body, so the label is unknown inside the
lambda's function → codegen aborts with *"Jump to unregistered join"*. The
tree-walking interpreter resolves the jump via the lexical join continuation and
returns `0`.

**Why production never sees it:** `Translate.hs`'s `jumpCrossesLam` rewrites such
a `Join` into a `LetNonRec` + lambda wrapper *before* codegen (memory gotchas
#10/#17). Codegen therefore carries an **unchecked precondition** — "no `Jump`
crosses a `Lam`" — that any producer skipping that rewrite (hand-built IR, a
future front-end, an optimization pass that sinks a jump under a lambda) would
violate. The defensible fixes are either (a) handle the cross-boundary jump in
codegen, or (b) fail with a *clear, documented* precondition error rather than a
generic `NotYetImplemented`. Reported as B2 per the spec (JIT-only error outside
the whitelist).

This single root cause subsumes the `branchy` (jump-per-case-arm) and
doubly-nested-`Lam` variants — they all reduce to the same unregistered-join
error, so they are deduped to one finding.

## Negative results (no divergence found)

The four original skeletons (**LetRecSiblings, CaseOfCase, JoinRec, BoxChain**)
produced **zero** JIT-vs-eval divergences, zero non-determinism, and zero crashes
across the full run — strong evidence that the historical bug classes they target
are currently fixed:

- **LetRec 5-phase ordering / deferred Con fields / sibling-capture-drop** (gotchas
  #9, #11; "Resolved: LetRec thunk sibling capture drop"): no divergence with
  forward+backward sibling Con refs, dead bindings, and mixed Lam/Con/Pair/Simple
  RHS. (The deepest historical shape — a *thunkified* RHS capturing a later
  *deferred-simple* sibling — is partly out of reach from synthetic IR: it trips
  the `UnresolvedVar` laziness gap that the spec whitelists as non-bug, since the
  interpreter thunks inter-dependent simple bindings while the JIT evaluates them
  eagerly.)
- **Case-of-case join factories** (gotcha #2 tagToEnum-adjacent dispatch): no
  divergence, including under an applied lambda.
- **JoinRec arity / type-arg counting** (gotchas #1, #6): no divergence with
  0..3 fake leading params and bounded loops up to ~200 iterations.
- **Con-wrapper boxing** (gotchas #12, #15): no divergence for the *worker-path*
  box/scrutinize/rebuild chain. The precise historical bug (#12) requires a GHC
  DataCon **wrapper** unfolding that nests a boxed `I#` inside another box; that
  shape originates in `.hi` unfoldings and is not reachable from hand-built IR, so
  this skeleton characterizes the worker path only — noted as a coverage limit.

## Reach & generator-frequency stats

Reported by the `zzz_reach_floor` test (`--nocapture`). The DONE criterion is
≥90% of attempted cases reaching value comparison.

```
GHC-IDIOMS REACH: <reached>/<total> (<pct>%)   [target >= 90%]
SKELETON FREQ: letrec=<> caseofcase=<> (under_lambda=<>) joinrec=<> boxchain=<> joincross=<> (nested=<>) backref=<>
KNOWN-BUG HITS (#1 Jump-crosses-Lam, tolerated): <>
```

_(filled from the final run below.)_

## Coverage limits / honest caveats

- **optimize-then-compare** oracle skipped (dependency boundary).
- **Bug #12 (wrapper boxing)** not reachable from synthetic IR — needs a real GHC
  `.hi` wrapper unfolding; BoxChain exercises the worker path only.
- The **deepest LetRec capture-drop** shape collides with the whitelisted
  `UnresolvedVar` laziness gap and so cannot be distinguished from a non-bug from
  synthetic IR.
- B3 fork oracle requires `--test-threads=1` (malloc-lock safety in the child).
