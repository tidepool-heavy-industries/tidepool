# E6 — tiered `canonicalizeDFlags` (dev report)

Item: E6 (`00-spec.md` / `LEDGER.md`). SEMANTICS-SENSITIVE — exposed unfoldings
change what extraction SEES, so this carries the highest gate bar in the wave.
This report supplies the reachability rule, the scope decision, the
measurement, the detection-power demonstration, and the gate receipts; the
wire-move ruling itself is the wave TL's (root ruled: ordinary wire-moving
change, ships as part of the redeploy set) and is not re-litigated here.

## What changed

`GhcPipeline.hs`'s `runNormalPipeline` per-module loop is now two passes:

- **PASS 1** (every module): `parseModule` → `typecheckModule` → `hscDesugar`.
  Unconditional — diagnostics still surface for every home module, and the
  desugared (pre-optimization) Core is what the reachability walk (below)
  runs over.
- **PASS 2** (only modules in the reachable closure): `core2core`
  (`canonicalizeDFlags`'s -O2 + `Opt_ExposeAllUnfoldings`/
  `Opt_ExposeOverloadedUnfoldings`). A module outside the closure never pays
  this step; its (unoptimized) `ModGuts` is used only to have contributed to
  the reachability walk, then discarded — its `mg_binds` never reach
  `allBinds`.

`mg_tcs` (TyCon/DataCon declarations) is collected from **every** compiled
module unconditionally, tier notwithstanding — `core2core` never touches
`mg_tcs` (it transforms `mg_binds` only), so this costs nothing and keeps D1's
constructor-metadata walk exactly as covered as before the tier.

`canonicalizeDFlags` itself is **unchanged** — still applied uniformly to
every module's typecheck/desugar dflags (those steps don't consume
optLevel/unfolding-exposure; only `core2core` does), so the "tier" is
entirely in PASS 2's conditional, not in the function.

## The reachable-module rule (written down before implementation)

A home module is REACHABLE from the target iff it IS the target, or its
DESUGARED Core is transitively referenced — via a real `Var` occurrence at
any depth — from the target module's own desugared Core.
(`GhcPipeline.reachableModuleClosure`/`moduleRefs`/`externalVarModules`.)

Computed on **desugared, pre-`core2core`** Core specifically, not on
renamer/typecheck-level "used name" tracking, and not on source-import
closure:

- **Source-import closure is the whole graph, always** — for a single-target
  `depanal`, every compiled module is by construction a transitive import of
  the target, so a source-level tier would be a no-op. Confirmed empirically:
  see the fixtures below.
- **Renamer/typecheck-level usage would be UNSOUND**: it would miss a module
  imported only for an orphan instance (`import Foo ()`) — the instance's DFun
  reference only becomes an explicit `Var` once dictionaries are resolved,
  which happens at desugar time, not before.
- **Desugared Core is sound**: by desugar time, typeclass/instance selection
  is already explicit dictionary-`Var` application, so nothing is hidden from
  the walk. This codebase defines no `{-# RULES #-}` anywhere (grep-confirmed
  empty across `haskell/lib` and `haskell/src`), so `core2core` cannot
  introduce a genuinely NEW cross-module reference invisible at the desugared
  stage — the desugared reference graph is a superset-or-equal approximation
  of what the fully optimized Core actually needs.

No bound-variable tracking is needed in the walk (unlike
`Translate.exprFreeVarKeys`): a Core `Var` occurrence already points at its
exact binder `Id`, resolved by the renamer/typechecker, so a local binder can
never be confused with an unrelated same-named import. Over-inclusion (a
`Var` for a DataCon worker, whose defining module needs no unfoldings to be
useful) is harmless — the only failure mode this item must avoid is
EXCLUDING a module the target's Core genuinely needs.

**Confirmed empirically, not just reasoned about**, on a 14-home-module
fixture (`import Tidepool.Prelude hiding (error); result = object ["a" .=
(1::Int)]`): the closure correctly separates pure re-export modules
(`Tidepool.Aeson`, `Tidepool.Prelude` — both just `import`+re-export, nothing
of their own for anything to reference) from the modules that actually DEFINE
what's used (`Tidepool.Aeson.Value`, `.Scientific`, `Tidepool.Data.Text` —
the real definers of `object`/`.=`/`Value`, correctly found reachable).

### The soundness argument's one unenforced assumption — now enforced

The argument above's last step rests on an EMPIRICAL absence: "this codebase
defines no `{-# RULES #-}` anywhere (grep-confirmed empty), so `core2core`
cannot introduce a genuinely NEW cross-module reference invisible at the
desugared stage." True today, hand-verified — but nothing was enforcing it.
A `{-# RULES #-}` pragma added to `haskell/lib` or `haskell/src` tomorrow
would silently invalidate the soundness argument (a rewrite rule can splice
in a call to an otherwise-unreferenced function during `core2core`, after
the reachability walk has already run and decided what gets optimized) —
and per the risk-profile finding below, no standing gate would catch the
resulting mis-exclusion either.

Closed with a guard, not left as an assumption: `tidepool-codegen/tests/
e6_no_rules_pragma.rs`, quick tier (`cargo nextest run -p tidepool-codegen
--test e6_no_rules_pragma`), a grep-shaped test over `haskell/lib`+`haskell/
src` for a real `{-# RULES` pragma (a line whose FIRST token after trimming
is `{-# RULES` — deliberately excludes `--`-comment/Haddock prose that
merely *mentions* RULES pragmas, as this file's own commentary does; verified
against a false positive on exactly that during development). Fails loudly,
naming the reason, the day one is added. Positive-controlled: a real `{-#
RULES "test-rule" forall x. id x = x #-}` appended to `Tidepool.FilePath.hs`
made the test fail with the same message; reverted before commit.

**Scope is home modules only, and the wave TL pushed on this specifically —
this is not a shortcut, it needed checking, and it checks out.** The invariant
E6 actually needs is not "no RULES pragmas exist" (that's a proxy) but
**`core2core` cannot ADD a home-module cross-reference absent from desugared
Core**. `canonicalizeDFlags` disables `Opt_FullLaziness`/`Opt_CprAnal` but
never touches rewrite rules (grep-confirmed: no `Opt_EnableRewriteRules`
reference in `GhcPipeline.hs`), so PACKAGE-defined RULES (base's fusion rules
and friends) genuinely DO fire during `core2core` here — the guard's
home-only scope has to answer for that, not just assert it. It does: a
RULE's RHS is typechecked and scope-resolved at the RULE's OWN DEFINITION
SITE. A package-defined RULE can therefore only name identifiers already in
scope in that PACKAGE module — other package code — because home modules are
supplied via `--include`/`importPaths` entirely outside package resolution
and are invisible to a package's own build. A package RULE can rewrite one
package call into another; it cannot conjure a reference to, say,
`Tidepool.Aeson.Value` — it has no way to name it. Checked the two
neighboring mechanisms the same argument has to survive: inlining a package
function can't introduce a home reference either, because a package
function's body can never contain one to begin with (same scope argument,
one level down) — inlining only propagates references already present in the
inlined body, never invents new ones; specialisation is the same shape
(a local specialised copy of an already-referenced function, not a new
external reference). And a rule that *eliminates* a reference doesn't
threaten the invariant — elimination only makes the desugared-stage graph a
strict superset of what's needed, and over-inclusion is already argued
harmless above. So home-module scope for the guard is exactly coextensive
with the actual risk, not a narrower, more convenient proxy for it — this
reasoning (not just its conclusion) is in the guard's own doc comment, so a
future reader evaluating a NEW mechanism (a simplifier pass, a flag change)
can check it against the stated invariant rather than against the grep.

## Scope decision: `runNormalPipeline` only, `runSessionPipeline` untouched

**This is a real scope reduction from "tier the whole pipeline," and it has a
consequence that must be stated in these terms, not just as a scope note:**
`isSessionScopeActive` is true — routing to `runSessionPipeline` — whenever
any `Val.G<n>` iface is injected, which is **every real dogfood turn from
turn 2 onward** (C1's finding). So this item's tier lands on turn-1-shaped
extracts (every one-shot `eval`, and turn 1 of every session) and does
**NOT** reach the session turns that dominate a real multi-turn session.
"Tiered the normal path" and "tiered the path real turns take" are different
claims; only the first is true here.

**Why scoped down.** `runSessionPipeline`'s PHASE 3 loop interleaves, for
"deferred" modules (target ∪ transitive Val-importers), a `core2core` →
`hscTidy`/`mkIfaceTc` → `addToHpt` sequence so a LATER module in the same
topologically-ordered loop can resolve an EARLIER deferred module from HPT.
The loop's own comment marks this ordering load-bearing (the whole reason
PHASE 3 recompiles every home module to full guts, rather than resolving
library calls from HPT ifaces, is a previously-observed silent-corruption
failure). A reachability tier fundamentally needs a two-pass structure
(desugar everything, THEN decide which get `core2core`, because whether a
leaf module is "reachable" depends on modules processed AFTER it in
topological order) — and restructuring that around PHASE 3's existing
interleaved HPT-registration timing, for a mechanism this item is not
chartered to touch, could not be verified safe within this item's gate
budget. Per the spec's own instruction ("if your tiering cannot guarantee
[the invariant], say so and stop") — said so, stopped, scoped down instead of
guessing. `runSessionPipeline` keeps `canonicalizeDFlags` applied
unconditionally to every module, exactly as before this item.

## Measurement — the win, and honestly, not the whole `core` phase

**Correction taken on board mid-item**: `core` = desugar + `core2core`
summed across every module (pre-existing bracket, unchanged by this item).
PASS 1 still desugars every module — that work is NOT removed, only
`core2core` is tiered. So the addressable fraction is `core2core`, not the
whole `core` phase, and the 62–69% figure from `01-c1-measurement.md`
describes both combined. Reported below split, not claimed whole.

Fixture: the same 14-home-module `Expr2.hs` above (`--include haskell/lib`).
Debug-profile binaries built from this worktree, `TIDEPOOL_TIMING=1`, 3 runs
each side, box load ~13.3–15.5 (1-min loadavg) across the whole capture
window — a single, fairly stable load regime (I did not independently
reproduce C1's two-load-arm robustness demonstration for this number; treat
the ratio below as measured under one contention level, not proven
contention-robust the way C1's headline ratio was).

Pre-E6 binary (git-stashed diff, rebuilt), run 1 cold-excluded (ghc_setup
127ms vs 28–35ms baseline — a warm-up outlier, not representative):

| run | ghc_setup | ghc_load | typecheck | core | total |
|---|---|---|---|---|---|
| 2 | 28 | 823 | 183 | 3028 | 4266 |
| 3 | 29 | 795 | 156 | 2869 | 4052 |
| mean | 28.5 | 809 | 169.5 | **2948.5** | 4159 |

Post-E6 binary, same fixture, same worktree, immediately after:

| run | ghc_setup | ghc_load | typecheck | core (=desugar+core2core) | desugar | core2core | total |
|---|---|---|---|---|---|---|---|
| 1 | 29 | 814 | 163 | 954 | 36 | 918 | 2152 |
| 2 | 35 | 815 | 163 | 924 | 37 | 887 | 2105 |
| 3 | 33 | 827 | 204 | 922 | 46 | 876 | 2190 |
| mean | 32.3 | 818.7 | 176.7 | **933.3** | 39.7 | **893.7** | 2149 |

`modules=14 reachable=4` in every post-E6 run (10 of 14 tiered to
validation-only: `Tidepool.Aeson`, `.FromJSON`, `.Lens`, `Tidepool.Data.Time`,
`Tidepool.FilePath`, `Tidepool.Prelude`, `Tidepool.QQ.Fmt.Runtime`,
`Tidepool.Records`, `.Bridged`, `Tidepool.Render`).

**Readings:**
- `ghc_load` and `typecheck` are unchanged (809→818.7ms, 169.5→176.7ms) —
  expected: neither is tiered, both still process every module.
- `core` (the whole bracket) fell 2948.5→933.3ms, **31.7% of its former
  size** (a 68.3% reduction).
- Within post-E6's `core`, desugar (unconditional, every module) is
  39.7ms — **4.3%** of the post-E6 `core` bracket; `core2core` (tiered) is
  893.7ms — **95.7%**.
- Estimating pre-E6's `core2core` alone (desugar cost should be ~constant
  either side, since it's not tiered and does the same work regardless):
  2948.5 − 39.7 ≈ 2908.8ms pre-E6, vs 893.7ms post-E6 — **`core2core`
  itself falls to 30.7% of its former size** (a 69.3% reduction, ~3.25×)
  when 10 of 14 (71%) of modules are tiered down. This is an estimate (no
  pre-E6 desugar/core2core split instrument exists — desugar and core2core
  were always bracketed together before this item), not a direct
  measurement; flagged as such.
- `total` fell 4159→2149ms, **51.7% of its former size** for this specific
  input (a ~1.94× speedup) — the input-dependent, not-comparable-to-other-
  fixtures headline: how much narrows entirely on how much of the import
  graph the tier can exclude, which varies per eval.

Diagnostic instrument (new, this item, `e6-tier ...` stderr line,
`TIDEPOOL_TIMING`-gated but deliberately OUTSIDE the `tidepool-timing
phase=...` wire grammar — confirmed `ExtractTiming::parse` ignores
non-`tidepool-timing`-prefixed lines, so nothing needed updating on the Rust
side): reports `modules=`/`reachable=`/`desugar_ms=`/`core2core_ms=`/
`validation_only=[...]`/`reachable_names=[...]` — this is what produced the
split above and is what a future reader can reach for to size the win on
their own fixture. Documented in `haskell/CLAUDE.md`'s diagnostics table
alongside `TIDEPOOL_TEST_FORCE_VALIDATION_ONLY` (below).

**Control — a fixture where nothing is excluded is byte- and time-inert.** A
2-home-module fixture (target + one locally-defined helper module, both
reachable) produces `reachable==modules` and **byte-identical** `result.cbor`
/`meta.cbor` pre- vs post-E6 (see wire status below) — the tier only costs
anything, in bytes or time, when it actually excludes something.

## Wire status (evidence, escalated to root already — not re-litigated here)

**Tiering moves the wire when it actually excludes ≥1 module; it is
byte-inert when nothing is excluded.** Full evidence, mechanism (`localVarId`
vs `stableVarId`, raw-`Unique` allocation-order sensitivity), and the
decisive pre-E6-only two-import-breadth experiment (context-dependence of
internal/floated ids PRE-EXISTS E6 — this is a new trigger for an existing
property, not a new class of instability) are in the chat log with the wave
TL; root ruled it an ordinary wire-moving change. Confirmed independently:
`TIDEPOOL_TIMING` on/off remains byte-inert on the post-E6 binary (unchanged
discipline).

## The three pinned id-stability tests

Run and reported by name, per the wave TL's explicit correction to how a PASS
here should be read: **a green result here is "these three properties are
intact," never "id stability is intact."** None of the three observes
`localVarId` (the mechanism the wire-move finding is about) — test 1 is a
different id space, test 2 asserts distinctness/correct-resolution under any
permutation (not numeric-value stability), test 3 pins the OTHER branch of
`varId`'s `isExternalName` split, the one that's compile-order-invariant by
construction. The decisive instrument for the wire question was the
two-import-breadth experiment above, not these three.

- `tidepool-repr::extend_checked_equivalence::distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order`
  — **PASS** (quick tier, 0.004s)
- `tidepool-repr::extend_checked_equivalence::merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard`
  — **PASS** (quick tier, 0.005s)
- `tidepool-runtime::session_table_qualified_identity::accumulated_session_table_keeps_one_id_per_qualified_name`
  — **PASS** (GHC-heavy, `ghc-slots.sh detach` via `scripts/battery.sh -p tidepool-runtime -E 'test(...)'`, 6.357s)

None fired. Per spec, a fire would have been a full stop and an escalation —
did not occur.

## Detection power — the mis-tiering, what caught it, and what didn't

**Fault:** `TIDEPOOL_TEST_FORCE_VALIDATION_ONLY=<module>` (new, mirrors D1's
`TIDEPOOL_TEST_DROP_DC` — env-gated, inert unless set, documented in
`haskell/CLAUDE.md`'s diagnostics table). Forcibly deletes one named module
from the computed reachable set regardless of what the real closure found —
simulating exactly the mistake this tier could make.

**Chosen fault: deny `Tidepool.Aeson.Value` on the `Expr2.hs` fixture**
(`object`/`.=`/`Value` are genuinely defined there and genuinely called by
`result`) — the most plausible real mistake per the wave TL's steer: a
careless implementation could believe `Tidepool.Aeson` (a pure re-export
facade) is the real boundary and miss that the actual definitions live one
module deeper, in `Tidepool.Aeson.Value`.

**None of the wave's standing named gates caught it — stated plainly, not
glossed over:**
- `extract-fidelity-test` ran **30/30 green with the fault injected**. Its
  fixtures (erasure symmetry, recognizer qualification, unboxed-tuple arity,
  D1 defense) never touch JSON/Aeson at all.
- `haskell_suite_differential` and `corpus_report` read STATIC, pre-generated
  `.cbor` fixtures from `haskell/test/suite_cbor/` and `.../corpus_cbor/` —
  neither re-invokes the extractor at test time, so an extractor-level env
  var has no path to reach them without regenerating those shared fixture
  directories (not attempted — real risk of leaving the shared fixtures
  corrupted for other in-flight work in this worktree, for marginal gain
  over the alternative below).

  This is itself a finding worth carrying forward: **the standing battery has
  a real coverage hole for Aeson/JSON-touching code** — nothing in it would
  catch a regression specific to that surface, tiering-related or not.

**What actually caught it, mechanism-grounded:** a temporary probe
(`tidepool-codegen/tests/e6_scratch_probe.rs`, deleted before this branch was
submitted — not part of the diff) reading a CBOR pair and running it through
`tidepool_testing::proptest::check_jit_vs_eval_captured` — the exact oracle
function `real_core_corpus.rs`/`corpus_report` call, not a new mechanism.

- Correctly-tiered `Expr2` output (`Tidepool.Aeson.Value` reachable):
  **`AGREE`**, a real `Value` (JSON object) result from both eval and JIT.
- Mis-tiered output (`Tidepool.Aeson.Value` force-denied): **`BOTH_FAIL
  eval=UserError jit=Yield(Runtime(TypeMetadata))`**, with `[JIT]
  runtime_error called: kind=4 (TypeMetadata) msg="a"` on stderr — the exact
  `kind=4 TypeMetadata` signature `runSessionPipeline`'s PHASE 3 comment
  predicts for this failure class, reproduced by name.

> **AMENDMENT (spawn-latency, 2026-08-09, post-fold — wave `ff06349c`). DO NOT
> READ THIS SECTION AS "kind=4 ⇒ mis-tiering".** The demonstration above is
> valid for the fault it injected, and nothing in E6's conclusion changes. But
> `kind=4 TypeMetadata` now has **at least TWO distinct known causes**, so the
> inference from SIGNATURE to CAUSE is unsound.
>
> The second cause: `Translate.isTypeMetadataVar` (`Translate.hs:2868`) matches
> on OCCURRENCE-NAME PREFIX ONLY — `["$trModule","$krep","$tc","krep$",
> "tr$Module"]` — with no RHS inspection. GHC's float-out/CSE gives
> Generic-deriving's `KnownSymbol` dictionary backing strings (plain `Addr#`
> literals for `"Two"`, `"main"`, …) those same prefixes, so a **load-bearing
> literal is poisoned as `ERROR_SENTINEL`** and dies downstream with `kind=4`
> plus a bad-pointer `0x0`. That is the generic-surface `==` crash, being fixed
> by a root dev.
>
> So someone hitting `kind=4` later and consulting this receipt could
> misattribute a prefix-poisoning bug to tiering, or the reverse. **Check
> `isTypeMetadataVar`'s state at the base you are testing against before
> attributing a `kind=4` to E6.**
>
> Two taxonomy instances stacked: a NAME-SHAPE CONVENTION trusted to establish
> what the RHS actually is, and — downstream of it, and the one that reaches
> this document — a SYMPTOM trusted to establish its CAUSE. The receipt was
> accurate when written and became over-diagnostic when the world gained a
> second cause. That is a receipt aging badly rather than being written badly,
> which is its own failure mode: nothing in the artifact changed.

### Risk-profile finding: there is no fallback safety net for this failure class

I had reasoned, before running the experiment above, that `load'` (PHASE 1,
unconditional and untouched by this item's tier) gives every home module
full -O2 exposed unfoldings before PASS 2 ever runs, so `resolveExternals`'s
existing iface-unfolding fallback (built for PACKAGE externals) might
transparently absorb a wrongly-excluded HOME module too — a plausible safety
story. **Tested, not assumed, and REFUTED**: the poison sentinel fires
exactly as documented; it does not.

**This is a property of E6's risk profile, not an anecdote about one failed
hypothesis: the tier's reachability computation is the ONLY barrier between
a wrong exclusion and silent-at-compile/loud-at-JIT corruption.** No
fallback mechanism in the pipeline catches a wrong exclusion on its behalf.
That is exactly why this item's gate bar sits where it does, and it applies
identically to any future reachability-narrowing item over this same Core
(D2's `RuntimeTypeClosure` in particular).

**Summary, stated at the strength the evidence supports:** the detection
instrument is real, decisive, and built on the same oracle the named gates
use — but it required constructing a probe; no existing named gate in the
standing battery would have caught this specific fault as shipped. Report
both halves; neither alone is the honest sentence.

> **"detection power demonstrated" ≠ "the battery would have caught it."**

## Gate receipts (actual N/N, not the spec's placeholder counts — corrected
mid-item by the wave TL: `26/26`/`24/24` were wrong, D1-A already moved the
fidelity total to 30)

- **`cargo nextest run` (quick tier, pure-Rust, unbrokered per the updated
  throttle policy — `nice -n 15 cargo nextest run -j 4`):** **1875 tests run:
  1875 passed, 9 skipped.** Zero failures. (1874 before adding the RULES
  guard below; +1 with it.)
- **`tidepool-codegen::e6_no_rules_pragma::no_home_rules_pragmas_in_extract_relevant_haskell_source`**
  (quick tier, included in the 1875 above; named separately per the
  named-guard rule): **PASS**. Positive-controlled — a real RULES pragma
  appended to `Tidepool.FilePath.hs` made it FAIL with the invariant-naming
  message; reverted before commit, re-confirmed green.
- **`extract-fidelity-test`** (`ghc-slots.sh detach` → `nix develop --command
  cabal test extract-fidelity-test`): **30/30 checks passed**, all
  pre-existing checks named in the log, including the four D1Defense checks
  by name (erasure symmetry, recognizer qualification, both unboxed-tuple
  arity checks, both barrier checks, D1's four mutation-defense checks).
  E6 adds no new fidelity checks of its own (its detection-power
  demonstration lives outside this suite — see above).
- **`haskell_suite_differential`** (`TIDEPOOL_EXPENSIVE_TESTS=1
  scripts/battery-shard.sh tidepool-codegen --no-fail-fast --run-ignored all
  -E 'test(haskell_suite_differential)' --success-output final`): **1 passed,
  0 failed.** `tested=349, compared=312, closure_skip=34, mismatch=0,
  both_error=0, jit_only_error=0, eval_jit_diverge=3, skipped=1`.
  **`COMPARED_FLOOR=300` — `compared=312`, above floor, both before and
  after this item are the SAME 312**, because this gate reads static
  `test/suite_cbor/` fixtures never regenerated by this item's binary — E6
  cannot move this number by construction, not merely "didn't."
- **`corpus_report`** (same shard shape, `-E 'test(corpus_report)'`): **1
  passed, 0 failed.** Same static-fixture invariance argument as above —
  this item's binary never regenerates `test/corpus_cbor/`.
- **Harness acceptance** (`scripts/battery-shard.sh tidepool-harness
  --no-fail-fast -E 'binary(/^acceptance_/)'`): **24 tests run: 24 passed (3
  slow), 0 skipped.**
- **`cargo check --workspace --all-targets`**: clean.

No inherited-red accounting needed: none of these runs touched
`tidepool-runtime`'s `mock_stack_lockstep` suite (the one standing expected
red), so the "exactly one inherited red" rule doesn't apply to anything
reported here.

## Done criteria, checked against this report

- `canonicalizeDFlags` tiered; validation-only modules pay parse+typecheck+
  (a necessary, unconditional) desugar, never `core2core` — **done, scoped to
  `runNormalPipeline`** (see scope decision above; `runSessionPipeline` is
  unchanged).
- Reachability rule written down before implementation, gate results read
  against it — **done**, see above.
- Detection power demonstrated, gate named — **done, with an honest
  qualification**: the demonstration is real and oracle-grounded
  (`BOTH_FAIL`/`kind=4 TypeMetadata`), but no EXISTING named gate in the
  battery caught it; a purpose-built probe on the real oracle did. Both facts
  reported, neither alone.
- Full gate set green at zero tolerance — **done**, all N/N above, zero
  failures anywhere.
- Win measured, not asserted — **done**, `core2core` estimated 69.3%
  reduction (30.7% of former size) on a 14-module/10-excluded fixture; single
  load regime, not independently demonstrated contention-robust the way C1's
  ratio was.
- Wire status stated with evidence — **done**, escalated to and ruled on by
  root already; summarized above, not re-argued.
