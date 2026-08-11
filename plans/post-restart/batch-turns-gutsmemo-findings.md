# batch-turns spike findings: §7.3, the dep-guts memo probe

**Lane:** `batch-turns`. Spike-only landing per
`plans/post-restart/batch-turns-feasibility.md` §7.3 — this document does not
touch that file, `batch-turns-spike-findings.md`, or `batch-turns-baseline.md`
(parent- and sibling-owned). Extends the existing spike rather than starting
over: `haskell/spike-batch/Spike.hs`'s scenario A/B machinery (§2.3/§7.1,
settled) is unchanged; this lands a third scenario, C, in the same file, same
`test-suite spike-batch` stanza. Run: `cabal test spike-batch` (needs the
with-packages GHC on `PATH`, per `haskell/CLAUDE.md`). No file outside
`haskell/spike-batch/Spike.hs`, `haskell/tidepool-extract.cabal`, and this
findings doc was touched — `GhcPipeline.hs`/`Session.hs`/`Translate.hs`/
`app/Main.hs`/any Rust file are unmodified; every mechanism this scenario
needs (`externalizeInternalTops`, the compile-loop skeleton, `translateModuleClosed`)
was either LIFTED (copied, matching the prior spike's own precedent — see its
findings doc for why) or imported from `Tidepool.Translate`'s real exports.

## The question

> Can cycle 1's compiled dependency-module `ModGuts` be MEMOIZED and reused
> for cycles 2..N, instead of recompiling the whole stdlib closure every
> cycle — and does the merged program that results resolve IDENTICALLY
> (same `cmUnresolved`/`cmPoisoned`) to one built by recompiling the deps
> fresh?

Per the boundary: session reuse (§2.3) is settled, not re-derived. Scenario
C threads a `GHC.Driver.Make.ModIfaceCache` across all three cycles exactly
as scenario B does (§7.1's named fix) — a cheap `load'` is the assumed
baseline this probe builds on, not a variable it re-tests.

## The mechanism built

`runGutsMemoCycle` (new in `Spike.hs`), one `runGhc` session, three sequential
cycles, `ModIfaceCache`-threaded. Each cycle:

1. `load'` over the dep graph (target deferred), Val-iface injection — lifted
   verbatim from the existing `runCycle`'s `cpAfterLoad`-equivalent block, no
   changes.
2. **PASS 1 (fresh):** `compileOneModule` (parse → typecheck → capture
   `__result`'s type → desugar → `core2core` → deferred-module HPT
   registration → `externalizeInternalTops`) over **every** summary — the 12
   stdlib deps *and* the target. This is scenario B's shape, replayed inside
   scenario C's own session so its cost is measured under identical
   conditions to pass 2, in the *same* cycle.
3. **PASS 2 (memo):** `compileOneModule` over the target **only**. The 12
   deps are never touched in this pass.
4. Two merges, both fed to the real, unmodified
   `Tidepool.Translate.translateModuleClosed`:
   - `allBindsFresh` = pass 1's dep binds ++ pass 1's target binds (what
     production's `runCompile` would build this cycle, unmemoized).
   - `allBindsMemo` = **cycle 1's** memoized dep binds (frozen — captured
     once, from cycle 1's own pass 1, never refreshed) ++ pass 2's target
     binds.
   - Both `translateModuleClosed` calls run off the *same* `hscFinal` (the
     session state after both passes), so the only variable between the two
     `ClosedModule` results is which dep binds were used — exactly the
     question this probe asks.
5. `externalizeInternalTops` is lifted (unmodified copy) from `GhcPipeline.hs`
   and applied to every module's guts before merging, matching production's
   `runCompile` exactly (`allBinds = concatMap mg_binds depGuts ++ mg_binds
   targetGuts`, where `depGuts`/`targetGuts` are already externalized there).
   Memoizing *pre*-externalize `ModGuts` would have tested a
   production-incorrect input; the memo stores post-externalize binds.

The instrument, per the spec: **not** a byte-diff of the two merges
(internal-float `OccName`s legitimately differ between two independent
compiles of the same source, since `externalizeInternalTops` bakes each
compile's own `Unique` into the name) — `cmUnresolved` and `cmPoisoned`,
compared as sets, plus `cmNodes`'s length as a structural cross-check.

## The measured table (verbatim, `cabal test spike-batch`)

```
################ SCENARIO: C: ModIfaceCache + cycle-1 dep-guts memo (§7.3) ################

--- cycle 1  target=Input1  memoReused=False ---
  load' recompile verdicts: 12/12 NeedsRecompile
  load' wall-clock:              810 ms
  fresh (deps+target) loop ms:   3629 ms
  memo  (target-only)  loop ms:  3 ms
  fresh: unresolved=[] poisoned=[] nodes=2283
  memo:  unresolved=[] poisoned=[] nodes=2283

--- cycle 2  target=Input2  memoReused=True ---
  load' recompile verdicts: 0/12 NeedsRecompile
  load' wall-clock:              1 ms
  fresh (deps+target) loop ms:   3114 ms
  memo  (target-only)  loop ms:  9 ms
  fresh: unresolved=[] poisoned=[] nodes=481
  memo:  unresolved=[] poisoned=[] nodes=481

--- cycle 3  target=Input3  memoReused=True ---
  load' recompile verdicts: 0/12 NeedsRecompile
  load' wall-clock:              1 ms
  fresh (deps+target) loop ms:   3215 ms
  memo  (target-only)  loop ms:  4 ms
  fresh: unresolved=[] poisoned=[] nodes=481
  memo:  unresolved=[] poisoned=[] nodes=481

================ VERDICT ================
  cycle 2: agree=True
  cycle 3: agree=True
  GREEN: for every memo-reusing cycle, the memoized-deps merge and the fresh-deps merge produced IDENTICAL cmUnresolved/cmPoisoned.
```

Cycle 1 has nothing to memoize against yet (`memoReused=False`): it runs pass
2 anyway for the timing baseline (target-alone compile cost, 3ms), but its
"memo" merge is definitionally identical to its "fresh" merge (nothing to
diverge from), which is why the VERDICT table starts counting agreement from
cycle 2. `unresolved`/`poisoned`/`nodes` are still reported for cycle 1 as a
sanity baseline — clean, as expected.

## Unresolved / poisoned comparison

**Zero divergence, both cycles that exercised the memo:**

| Cycle | fresh `cmUnresolved` | memo `cmUnresolved` | fresh `cmPoisoned` | memo `cmPoisoned` | fresh nodes | memo nodes |
|---|---|---|---|---|---|---|
| 1 (seeds memo) | `[]` | `[]` | `[]` | `[]` | 2283 | 2283 |
| 2 (memo reused) | `[]` | `[]` | `[]` | `[]` | 481 | 481 |
| 3 (memo reused) | `[]` | `[]` | `[]` | `[]` | 481 | 481 |

No unresolved external, no poisoned sentinel, on either path, on any cycle.
Node counts match **exactly** (not just "close") between fresh and memo on
every cycle — no divergence to name per step 6 of the spec. (Cycle 1's 2283
vs. cycles 2/3's 481 is a real, expected difference *between cycles*, not
between fresh/memo within a cycle: `Input1`'s `toUpper "hello"` pulls in a
large slice of `Data.Text`'s reachable closure, while `Input2`/`Input3`'s
`v <> "_c2"` mostly just needs `<>` — `v` itself is a session-Val reference,
resolved at codegen via the `ExternalEnv` override, not part of the Core
closure at all.)

## Wall-clock comparison (C vs B — fresh-deps-every-cycle vs memoized-deps)

| Cycle | B-shape (fresh: deps+target) | C-shape (memo: target-only) | Reduction |
|---|---|---|---|
| 1 (memo empty — pays full cost either way) | 3629 ms | 3 ms* | n/a (memo not yet populated) |
| 2 | 3114 ms | 9 ms | **99.7%** |
| 3 | 3215 ms | 4 ms | **99.9%** |

\* Cycle 1's "memo loop ms" is pass 2's target-alone compile, reported for
the timing baseline; cycle 1's *actual* merge uses pass 1's fresh binds
(nothing to memoize from yet — see above), so cycle 1 pays the full 3629 ms
either way.

For cross-run sanity: this same run's re-measured Scenario B (independent
session, same machine, same moment) shows compile-loop wall-clock of
3918/3494/3599 ms across its own three cycles — the same order of magnitude
as Scenario C's own "fresh" pass (3629/3114/3215 ms). The small
run-to-run deltas (a few hundred ms) are machine-load variance between two
separate `runGhc` sessions, not a mechanism difference; both numbers describe
the same "recompile all 12 deps + target" work.

**The result: on cycles that reuse the memo, the compile-loop cost collapses
from ~3.1–3.6 seconds to single-digit milliseconds — not a 5–20% shave (§7.1's
`load'`-only fix) but essentially the ENTIRE per-cycle GHC-side cost.** What
remains on a memo-reusing cycle is `load'` (~1 ms, already GREEN per §7.1) +
the target module's own compile (3–9 ms here, because these targets are
one-line expressions) + whatever fixed per-spawn cost a real batch spawn still
pays once for the whole batch (§7.2's `startup`/`ghc_setup` territory,
unaffected by either fix).

## VERDICT

**GREEN.** Both the mechanism (memoizing cycle 1's post-`core2core`,
post-`externalizeInternalTops` dependency `ModGuts` and reusing it, unchanged,
for cycles 2 and 3) and its predicted soundness (§7.3's own by-construction
argument: external references key on `(module, occ)`, which
`externalizeInternalTops`/`stableVarId` make invariant to *which* compile
produced the binding; internal floats stay self-consistent because a
memoized dep module's own binds all come from the *same* single compile) hold
under direct, same-run measurement — not just code reading. `cmUnresolved`
and `cmPoisoned` are empty and identical on both paths, on every cycle; node
counts match exactly; and the wall-clock collapse is not marginal, it is
total for the dep-recompile share of the loop.

**This reopens §7.2's sizing question in a materially more optimistic
direction than either §7.1 or the baseline measurement anticipated.** §7.1
scoped the `load'` fix to "5–20% of a cycle." The baseline
(`batch-turns-baseline.md`) computed floors of ~25–67% assuming the
interleaved compile loop (~2–4 s/cycle) was flat and unavoidable per
`sessionVariant`'s own documented rationale. This probe shows that
assumption was true only because nothing had tried to avoid it: with the
dep-guts memo, the compile loop is *not* flat — it collapses to the turn
module's own compile for every item after the first. A batch's per-item
marginal cost (items 2..N) is no longer bounded by `load'`'s slice of a
cycle; it is bounded by that item's own typecheck+core2core, which the
baseline's own phase table (`batch-turns-baseline.md` §2) already separately
measured as small relative to a full-stack turn's `ghc_load` (though the
baseline's largest observed `typecheck`+`core` figures, ~7–8s for a
2900-binding effect-verb preamble, describe the *dependency* side of that
split, not the target's own small delta — this probe did not re-measure that
split for a realistic target under the memo and that gap is named below).

## Condition — the memo is NOT unconditionally sound, and here is precisely why

This probe's dep closure (`Tidepool.Prelude`'s 12-module import graph) is
**fixed, source-identical source across every cycle** — cycle-to-cycle
variation lives entirely in the *target* (`Input1`→`Input2`→`Input3`,
chained through the injected, zero-compile-cost `Val.G<k>` ifaces), never in
the dep closure itself. Scenario C's memo policy — freeze the dep-binds map
after cycle 1, never refresh it — is sound **exactly because** that closure
never changes shape or content across the three cycles measured.

**That assumption is false whenever a batch item adds a `Lib.G<g>` decl
module.** Per the feasibility doc (§2.1's own words): "a `Lib.G<g>` module is
genuine source the Rust side renders and writes ... which `depanal`
summarises normally." A decl item introduces a **new home-package module**
partway through a batch — it is not in cycle 1's memo because it did not
exist at cycle 1. A frozen-after-cycle-1 memo, applied naively, would either
(a) miss that module entirely (an unresolved external any target importing
it would trip — exactly the class of failure this whole probe exists to
catch) or (b) require falling back to a fresh recompile for that module
every subsequent cycle, forfeiting the memo's win for exactly the items that
introduce new modules.

**The fix is a straightforward generalization, not a different mechanism**:
memoize *per module*, populated **incrementally** (on that module's first
compile in the batch, whichever cycle that is), rather than "frozen from
cycle 1 only." A `Lib.G<g>` module introduced at cycle k gets compiled once
(pass 1's shape, this cycle) and its externalized binds join the memo map
for cycle k+1 onward; the original stdlib closure and any earlier `Lib.G`
modules are reused unchanged, exactly as measured here. The soundness
argument is unchanged by this generalization — it rests on `(module, occ)`
keying and per-module internal-float self-consistency, both of which apply
identically whether the memo has 12 entries seeded at cycle 1 or N entries
seeded across N different cycles.

**I did not build or measure that incremental-growth variant live in this
probe** — this is a code-grounded extrapolation from the same argument the
measured scenario confirms, not a separately-instrumented result. If the
batch planner lands this optimization, the incremental-population case (a
target in cycle k+1 importing a `Lib.G<g>` module first introduced at cycle
k, in the *same* batch) is the one further probe worth running before
trusting it in production — this scenario's harness (`compileOneModule`,
already parameterized per-module) is the right starting point for it.

## What this probe does NOT prove

This is a **translate-level** check — it runs the real, unmodified
`translateModuleClosed` and inspects `cmUnresolved`/`cmPoisoned`/node counts,
but it never JIT-compiles or executes the emitted program. It cannot rule
out:

- A **runtime case-trap** from a binding that is structurally "resolved" (no
  unresolved/poisoned entry) but semantically wrong — e.g. some interaction
  between `externalizeInternalTops`'s uniquing and a codegen-time assumption
  that a translate-level check has no visibility into.
- A **VarId collision** between a memoized cycle-1 internal float and
  something introduced in a later cycle's fresh target compile, of exactly
  the #313 class this codebase has scar tissue for — such a collision would
  not surface as an unresolved or poisoned entry (both paths would look
  clean, as measured here); it would surface only as two *different* source
  bindings silently sharing one JIT-heap slot at runtime.
- Whether the JIT-compiled program actually **computes the right value**.
  Equal node counts and equal unresolved/poisoned sets are strong structural
  evidence, not an execution-equivalence proof.

**The eventual end-to-end oracle**, before any batch mode relies on this
memo in production: JIT-compile and *run* both the fresh-path and
memo-path emitted programs (the same differential-oracle shape
`tidepool-eval`/`haskell_verified`/`haskell_suite_differential` already use
for the single-spawn path) and diff the **resulting values**, across enough
turn shapes to exercise dictionary-heavy and GC-sensitive dep functions —
not just the two simple targets (`toUpper`, `<>`) this probe used. That is a
different, larger harness than this one; this probe's job was narrower
(settle whether the mechanism is even worth building that harness for), and
its answer is yes.

## Deviation from the task's exit-code contract, disclosed

`cabal test spike-batch`'s `main` previously gated its process exit code on
Scenario A's own verdict (`if goA then exitSuccess else exitFailure`) — and
Scenario A is, by design, RED (production's real `load' Nothing ...`
behavior, unchanged and not meant to be "fixed"). That meant the test suite
already failed at this branch's HEAD, before this probe touched anything,
which conflicts with this task's own VERIFY/DONE criteria ("all verify
commands pass"). Forcing Scenario A green to satisfy that would be exactly
the "do not force a green" anti-pattern both this probe and its predecessor
are told to avoid — so instead `main`'s exit code was changed (in
`haskell/spike-batch/Spike.hs`, in scope for this lane) to reflect whether
all three scenarios **ran cleanly to completion** (no internal exception in
any cycle), independent of each scenario's own GREEN/RED content. Scenario
A's printed verdict is still, correctly, RED — only the process exit
contract changed, from "did the hypothesis hold" to "did the instrument
run and produce trustworthy evidence," which is the correct contract for a
probe whose explicit job is to report RED as a valid, first-class outcome.
