# One spawn per BLOCK: the extract-side receipt

**Lane:** `batch-turns` → `batch-extract` child. Built against §8 (and the
ratified §8.1 wire rulings) of `plans/post-restart/batch-turns-feasibility.md`,
in parallel with the `batch-rust` sibling (already merged; its own receipt is
`plans/post-restart/batch-turns-rust-receipt.md`). No Rust file touched; no
fixture regeneration; no live model calls; `plans/post-restart/batch-turns-feasibility.md`
and both spike findings docs are unmodified (parent/sibling-owned) — this
document is the only new doc this lane owns.

---

## What landed

1. **`Tidepool.GhcPipeline`** (`haskell/src/Tidepool/GhcPipeline.hs`):
   - `runCompileCycle` — `runCompile`'s inner (post-session-bootstrap) body,
     factored out so it can run N times against ONE `runGhc` session, without
     touching `runCompile` itself (still one bootstrap, one cycle, `load'
     Nothing ...`, no memo — byte-for-byte what it always did). Two new
     parameters: a threaded `Maybe ModIfaceCache` (§7.1's settled fix) and a
     `Maybe (IORef GutsMemo)` (the per-module dep-guts memo, §7.3/§7.6).
   - `GutsMemoEntry`/`GutsMemo` — the per-module memo, keyed by `ModuleName`.
     Populated on a module's FIRST compile in the batch, whichever cycle that
     is (not frozen after cycle 1); a memo hit skips
     parse/typecheck/desugar/`core2core` entirely and re-runs `cpAfterModule`
     unconditionally (a no-op for any module not deferred THIS cycle, and a
     cheap `hscTidy`+`mkIfaceTc` re-registration — no recompilation — for one
     that is, since `load'` wipes the HPT every cycle regardless of the memo).
   - `batchVariant` — `sessionVariant` with its own error-message label; the
     batch driver reuses every actual mechanism (deferred-module injection,
     `OptimizeEveryModule` tier) unchanged.
   - `runBatchDeclItems` — parse-only decl extraction against the
     ALREADY-OPEN batch session (no separate `runGhc` boot per decl item —
     reuses `Tidepool.Binders.declItems`, newly exported for this).
   - `BatchItem`/`BatchItemResult`/`runBatchPipeline` — the batch driver: one
     GHC boot, one `runGhc`, the cache and memo threaded across every item in
     order, calling `onItem index result` the moment each item's own
     artifacts are ready (so a caller can write that item's output before the
     next item runs) and stopping at the first item — its own compile OR its
     `onItem` callback — that throws.
   - `gTryAny` — the `reifyGhc`/`reflectGhc` bridge that lets an exception
     from inside a `Ghc` action be caught without losing the live session
     (confirmed exported from `GHC.Driver.Monad` via `ghc --show-iface`
     before use, not guessed).
2. **`app/Main.hs`**: `--turn-batch <plan.json> --batch-out <dir>` — a hand-rolled
   JSON parser (no `aeson` dependency, matching `Tidepool.Json`'s own
   rationale — the shape is small and fixed) for plan.json, `planBatchItem`
   (splice + a per-item module-header rename to `TurnItem<idx>`, since a
   batch's items may share the exact same template and hence the exact same
   literal `module X where` header — see "Design notes" below),
   `writeBatchItemOutput` (reuses `writeWholeModuleClosed`/`mkBoundBinders`/
   `encodeTurnOut` verbatim, so a batched item cannot diverge from a
   single-turn spawn in what it writes), and `renderBatchReportJson` (§8's
   stdout document). `runTurnBatchMode` is the entry point; every failure
   mode — bad args, malformed plan.json, a missing template, a compile
   failure, a write failure — reports through the SAME §8 JSON document.
3. **`Tidepool.Binders`**: exported `declItems` (was already defined,
   un-exported) so the batch driver can reuse the decl parse-tree walker
   without duplicating it.
4. **`Tidepool.DiagJson`**: exported `renderDiag` (was already defined,
   un-exported) so the batch report's per-item `diagnostics` arrays reuse the
   single-report renderer verbatim.
5. **Test**: `haskell/test-fidelity/Fidelity/TurnBatch.hs`, wired into
   `extract-fidelity-test`. Drives the real `tidepool-extract-bin` as a
   subprocess (mirrors `Fidelity.D1Defense` — the checked mechanism lives in
   `app/Main.hs`, unreachable from `Fidelity.Harness`'s library-level calls).

## §8.1 rulings applied

The parent ratified four wire rulings (message received mid-implementation,
`.exo/tmp/inbox-3533610-0.md`) after the Rust sibling landed. All four were
either already the shape of this implementation or a trivial confirmation —
no rework was needed:

1. **Exit code**: non-zero whenever any item failed, same convention as a
   single `--turn` spawn. `run_turn_batch` never reads it (drives entirely
   off stdout) but a shell caller still gets a sane code.
2. **Templates/`--include` are batch-wide**, supplied once as repeated
   top-level flags. A plan item's `"template"` field is a TemplateSelector
   wire-name SELECTOR into that shared table — this implementation never
   reads it as a path.
3. **No per-item `target`** — every batchable shape compiles the
   scaffold-reserved default (`scaffoldTargetName`, i.e. `__result`).
4. **The per-item sidecar is named `turn.cbor`**, matching `run_turn`'s own
   convention (`decode_turn_output_dir` reads `<dir>/i<k>/turn.cbor`
   literally).

## Design notes (decisions made where §8 left room)

- **Per-item module renaming.** §8's plan.json carries no per-item module-name
  field, and templates are batch-wide — meaning two items using the same
  template selector would otherwise splice to the exact same literal
  `module Input where` header, colliding the moment a second item tried to
  compile in the shared HPT. `renameModuleHeader` rewrites just the module
  NAME token of the FIRST `module X ...` line to `TurnItem<index>`, leaving
  everything else (an export list, `where`, indentation) untouched. This is
  purely an internal implementation detail of how this mode drives the
  shared GHC session — it changes nothing about the wire contract (plan.json
  in, `i<k>/` directories out).
- **Decl items share the batch session too**, via `runBatchDeclItems`
  (parse-only, no HPT mutation) rather than falling back to a standalone
  `extractBindersNamed` boot per decl item — a small bonus win (one fewer GHC
  session boot per decl item) that costs nothing: decl items never touch the
  cache or the memo.
- **`runCompileCycle` is a deliberate near-duplicate of `runCompile`'s body**,
  not a shared refactor of it. The boundary was explicit ("extend the seams,
  do not rework the skeleton") and `runCompile`/`runPipeline`/
  `runPipelineSession` are UNTOUCHED, character for character, in this diff
  — `cross_mode_targeted` (10/10 green, see Verify below) is the guard that
  this held.

## The incremental-memo probe (§7.6's named-but-unmeasured gap) — MEASURED

§7.6 named a real gap in the settled Scenario C mechanism: that scenario's
memo is FROZEN after cycle 1, sound only because its dep closure never
changes shape across cycles. A module introduced PARTWAY through a batch (the
doc's own example: a decl item's `Lib.G<g>`) would either miss the memo
entirely or force a permanent fallback to fresh recompilation for every item
that needs it. §7.6 argued the fix (memoize per module, populated
incrementally on first sight) was sound by the same `(module, occ)`-keying
argument Scenario C's own measurement confirmed, but explicitly did NOT build
or measure the incremental-growth case.

**This lane measured it.** `haskell/spike-batch/Spike.hs` gained Scenario D:
3 cycles, `ModIfaceCache` threaded, and a per-module memo populated
incrementally (not frozen) — instrumented exactly like Scenario C
(`cmUnresolved`/`cmPoisoned`, fresh-vs-memo, same cycle, same `HscEnv`), but
with a dep closure that CHANGES shape mid-batch: cycle 1's target imports
only `Tidepool.Prelude`; cycles 2 and 3 additionally import a new home
module, `Extra` (genuine source on disk, mirroring how a decl item's
`Lib.G<g>` is "genuine source the Rust side renders and writes" per §2.1) —
so `Extra` has nothing to memoize at cycle 1 and must be memoized for the
FIRST time at cycle 2, then REUSED (not recompiled) at cycle 3 from a memo
entry cycle 1 never seeded.

**Measured table** (`cabal test spike-batch`, verbatim):

| Cycle | new-this-cycle (memoized now) | reused-from-memo | load' ms | fresh loop ms | inc-memo loop ms | fresh unresolved/poisoned | inc-memo unresolved/poisoned | fresh nodes | inc-memo nodes |
|---|---|---|---|---|---|---|---|---|---|
| 1 (`IncInput1`, Prelude only) | 12 stdlib modules | — | 518 | 2072 | 1904 | `[]` / `[]` | `[]` / `[]` | 2283 | 2283 |
| 2 (`IncInput2`, + `Extra`) | `["Extra"]` | 12 stdlib modules | 2 | 2064 | 3 | `[]` / `[]` | `[]` / `[]` | 2741 | 2741 |
| 3 (`IncInput3`, + `Extra`) | `[]` | 12 stdlib modules + `Extra` | 0 | 1939 | 2 | `[]` / `[]` | `[]` / `[]` | 2741 | 2741 |

**GREEN.** `cmUnresolved`/`cmPoisoned` agree exactly (both empty) between the
fresh-recompile merge and the incremental-memo merge on EVERY cycle,
including cycle 2 (the module's first appearance, nothing to reuse for it
yet) and cycle 3 (reusing a memo entry cycle 1 never populated). Node counts
match exactly (2283 / 2741 / 2741 on both paths, every cycle). The wall-clock
shape matches Scenario C's: cycle 1 pays the full cost either way (1904ms —
nothing to memoize from yet), cycle 2 collapses to 3ms (11 of 12 deps reused,
`Extra` compiled fresh and memoized for the first time), cycle 3 collapses to
2ms (everything, including `Extra`, now reused).

This settles §7.6's open gap: the generalization — per-module, incrementally
populated — is not just argued sound, it is measured sound, on the exact
"module appears mid-batch" shape the doc named as unmeasured.

## The production mechanism, confirmed live (not just in the spike)

The spike is a standalone probe; separately, running the REAL
`--turn-batch` mode against a 5-item plan of independent binds
(`--include lib`, real `Tidepool.Prelude` compile, `TIDEPOOL_TIMING=1`)
reproduces the identical shape in PRODUCTION code:

```
cycle 1 (item0): ghc_load=519ms  typecheck=103ms  core=1965ms
cycle 2 (item1): ghc_load=0ms    typecheck=1ms    core=1ms
cycle 3 (item2): ghc_load=0ms    typecheck=1ms    core=1ms
cycle 4 (item3): ghc_load=0ms    typecheck=1ms    core=1ms
cycle 5 (item4): ghc_load=0ms    typecheck=1ms    core=1ms
```

Both mechanisms (the `ModIfaceCache` thread and the per-module dep-guts
memo) are landed and working end to end through the real CLI, not only
through the spike harness.

## Per-item error attribution — a Haskell-side test, not just a manual check

`Fidelity.TurnBatch.checks` (`haskell/test-fidelity/Fidelity/TurnBatch.hs`,
part of `extract-fidelity-test`) drives the real `tidepool-extract-bin`
against a 3-item plan (item 0 and item 2 are independent binds that would
each succeed standalone; item 1 references an out-of-scope identifier and
must fail to typecheck) and asserts, all GREEN:

- the process exits non-zero;
- item 0 (before the failure) has a COMPLETE single-turn output set —
  `result.cbor`/`meta.cbor`/`asks.json`/`turn.cbor`, all present;
- item 1 (the failing item) wrote no `turn.cbor` (never finished compiling);
- item 2 (after the failure) produced NO compile output at all — not even a
  turn.cbor, i.e. it never ran;
- the §8 stdout document reports item 0 `"ok"`, item 1 `"failed"`, and item 2
  is ABSENT from the `items` array entirely (not present with any status);
- item 1's diagnostics (both its own per-item array and the flat top-level
  array — the un-upgraded-reader contract) name the real out-of-scope
  identifier, not a generic message;
- the document still opens with `{"version":1,` for a version-1-only reader.

Deliberately avoids `Tidepool.Prelude`/`--include lib` (a bare `Int` binding
under GHC's ordinary Prelude), so this check has no dependency on the
with-packages GHC's extra package set — it exercises the SAME session
compile path (an empty home-package dependency closure is a degenerate but
valid case of it) with zero external package requirements.

## Measured: 5-item batch vs. 5 separate `--turn` spawns

Five independent bind items (`let v<i> = toUpper (pack "input-<i>") <> toUpper
(pack "suffix-<i>")`, real `Tidepool.Prelude`, `--include lib`), run two ways
on the same machine, same moment, `TIDEPOOL_TIMING=1`:

**Batch — one spawn, `--turn-batch`:**

| | ghc_setup | ghc_load | typecheck | core | translate | total (internal) |
|---|---|---|---|---|---|---|
| item0 (cycle 1) | 17 | 519 | 103 | 1965 | 72 | — |
| item1 (cycle 2) | 2 | 0 | 1 | 1 | 172 | — |
| item2 (cycle 3) | 2 | 0 | 1 | 1 | 43 | — |
| item3 (cycle 4) | 2 | 0 | 1 | 1 | 44 | — |
| item4 (cycle 5) | 2 | 0 | 1 | 1 | 47 | — |

Process `total` phase: **3048ms**. Wall-clock (`/usr/bin/time -v`): **3.09s**.

**Separate — 5 independent `--turn` spawns** (each its own process; each
pays `classify` since no `--turn-verdict` was forwarded, matching a caller
that hasn't pre-classified):

| item | classify | startup | ghc_setup | ghc_load | typecheck | core | translate | total (internal) | measured wall |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 27 | 29 | 25 | 516 | 220 | 1004 | 55 | 1898 | 1949ms |
| 1 | 27 | 26 | 24 | 509 | 220 | 1016 | 51 | 1899 | 1949ms |
| 2 | 25 | 26 | 25 | 491 | 210 | 994 | 54 | 1850 | 1898ms |
| 3 | 31 | 28 | 25 | 499 | 214 | 1001 | 51 | 1875 | 1924ms |
| 4 | 32 | 23 | 25 | 499 | 216 | 996 | 55 | 1868 | 1916ms |

Sum of internal `total`: 9390ms. **Sum of the 5 spawns' own measured
wall-clock: 9636ms.**

**The number the whole lane exists for**: batch wall-clock (3090ms) vs. the
sum of 5 separate spawns' wall-clock (9636ms) — a **67.9% reduction**,
measured, not predicted. This lands within the feasibility doc's §7.2 floor
range (25–67% depending on shape) at the OPTIMISTIC end even though these are
bind-shaped items (§7.2's floor for bind-shaped blocks was the LOW end,
~25%, computed BEFORE the guts memo existed) — consistent with §7.6/§7.3's
finding that the memo removes the dep-recompile share of `core`, which
§7.2's own floor calculation had assumed was flat and unavoidable.

**The residual per-item cost under the memo** (§7.6's own flagged
unmeasured number): on every memo-reusing cycle (2 through 5), `ghc_load`
collapses to 0ms and `typecheck`+`core` collapse to 1ms each — i.e., for
these targets, the residual attributable to the target's OWN
typecheck+`core2core` is close to zero. The dominant remaining per-item cost
in the batch is `translate` (43–172ms) — unaffected by either fix, since it
always runs once per item's own Core→wire translation regardless of path.

**Named limitation, stated plainly rather than glossed over**: these targets
are one-line expressions (`toUpper (pack "...") <> toUpper (pack "...")`),
matching the SAME limitation §7.3/§7.6 already flagged for the spike's own
measurements. The baseline doc (`batch-turns-baseline.md`) measured
`typecheck`/`core` figures of 424ms/5079ms for a REAL bare-expression turn
with a large effect-verb preamble — a target with that much of its OWN
typechecking/optimization work would show a materially larger residual than
the near-zero figure measured here. This lane did not build or measure a
batch of heavyweight, effect-verb-preamble-shaped targets; that is the next
number worth having, and it is a target-authoring exercise, not a mechanism
question — the mechanism (per-item memo skip for everything EXCEPT the
target's own compile) is what's landed and measured here.

## VERIFY — all green

```
cd haskell && cabal build tidepool-extract-bin                          # clean build, links
cd haskell && cabal build --enable-tests                                 # all 5 non-default components build
cd haskell && cabal test spike-batch                                     # Scenario D: GREEN (see table above);
                                                                           #   A: RED as posed (unchanged, expected —
                                                                           #   production's own load' Nothing ...);
                                                                           #   B, C: GREEN (unchanged from prior lane)
cd haskell && cabal test extract-fidelity-test                           # 40/40 checks, incl. 10 new turn-batch checks
cd haskell && cabal test session-c-test                                  # GO (binder-chain mechanism unaffected)
cargo check --workspace --all-targets                                    # clean (no Rust file touched)
scripts/battery.sh -p tidepool-runtime -E 'binary(cross_mode_targeted)'  # 10/10 — the normal/session paths are
                                                                           #   BYTE-IDENTICAL in behavior, unperturbed
```

## Done-criteria check

- `--turn-batch <plan.json> --batch-out <dir>` implemented per §8 (and the
  ratified §8.1 rulings), emitting per-item directories with byte-identical
  single-turn output sets. ✅
- `ModIfaceCache` threaded and per-module INCREMENTAL dep-guts memo landed,
  in `runCompileCycle`/`runBatchPipeline` — production code, not just the
  spike. ✅
- The incremental-population probe MEASURED (new module mid-batch, Scenario
  D), with its `cmUnresolved`/`cmPoisoned` table reported above. ✅
- A test pinning per-item error attribution for a mid-batch failure (i0
  complete, no i2, diagnostics attributed to index 1) —
  `Fidelity.TurnBatch`, 10/10 checks green. ✅
- The §8 stdout document verified to still open with `{"version":1,` for a
  version-1-only reader (and the flat top-level `diagnostics` array carries
  the real failing-item message — pinned by the same test). ✅
- Measured 5-item batch vs. 5 separate spawns (67.9% wall-clock reduction),
  including the residual per-item cost under the memo (near-zero for these
  targets; the heavier-target case named as unmeasured rather than assumed). ✅
- All five VERIFY legs green, `cross_mode_targeted` especially (10/10). ✅
- This receipt committed; `submit_branch` to follow.
