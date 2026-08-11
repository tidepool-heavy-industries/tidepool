# GhcPipeline: seam analysis for unifying the twin compile pipelines

Written BEFORE the refactor (lane `pipeline-unify`, approved Inanna 2026-08-10),
against `haskell/src/Tidepool/GhcPipeline.hs` at commit `a523f934`.

## Why this lane exists

`runNormalPipeline` and `runSessionPipeline` are two hand-maintained copies of
the same compile skeleton. Every fix lands twice. Item 20 (commit `e213ad29`)
is the live proof: the hs-boot `HsSrcFile` filter had to be added at TWO sites,
and the session copy was carrying the *latent* form of the exact bug the normal
copy had already expressed. The unification's success criterion is not line
count — it is that the next summary-handling fix is structurally a one-site
change.

## Line-by-line classification

`N` = `runNormalPipeline` (lines 114-380), `S` = `runSessionPipeline`
(lines 449-656) in the pre-refactor file.

### Skeleton — identical in both, becomes ONE site

| What | N | S |
|---|---|---|
| `readTimingEnabled` + `getLibdir` timed → `startup` phase | 116-118 | 451-453 |
| `runGhc`, `monotonicTime` t0, `getSessionDynFlags`, `setSessionDynFlags (extractionDynFlags …)` | 119-140 | 454-457 |
| `guessTarget` / `setTargets` | 141-142 | 458-459 |
| `warnRef` + `pushLogHookM (warnCollectorHook path …)` | 146-147 | 462-463 |
| `depanal … False` | 168 | 469 |
| `ghc_setup` phase emit | 175-176 | 475-476 |
| `unpoison` (`gopt_unset Opt_IgnoreInterfacePragmas` per summary) | 183-184 | 477-478 |
| `load' Nothing LoadAllTargets mkUnknownDiagnostic (Just batchMsg) (mapMG unpoison …)` | 186-187 | 521-522 |
| `ghc_load` phase emit | 192 | 527 |
| **hs-boot `ms_hsc_src ms == HsSrcFile` filter** | 203-204 | 576-581 |
| empty-module-graph guard | 205-206 | 582-583 |
| `tcMsRef` / `coreMsRef` accumulators | 223-224 | 584-585 |
| per-module `canonicalizeDFlags (ms_hspp_opts …)` | 248 | 587 |
| per-module `timeSection (parseModule >>= typecheckModule)`, summed into `tcMsRef` | 249-252 | 593-596 |
| `getSession` + `hscUpdateFlags canonicalizeDFlags` | 253-254 | 597-598 |
| `fst (tm_internals_ typechecked)` | 255 | 599 |
| `capturedUserType tcGblEnv` | 259 | 600 |
| `hscDesugar hscEnv modSum tcGblEnv` | 268 | 607 |
| `core2core hscEnv desugared`, summed into `coreMsRef` | 313-314 | 608-609 |
| `externalizeInternalTops` | 316 | 632 |
| `typecheck` / `core` phase emits | 320-321 | 635-636 |
| `isTargetMod` / `fst3` / target selection | 355-362 | 637-644 |
| `allBinds = concatMap mg_binds depGuts ++ mg_binds targetGuts` | 369 | 645 |
| `getSession`, `nub . reverse <$> readIORef warnRef`, `PipelineResult` | 371-380 | 647-656 |

That is the overwhelming majority of both bodies.

### Genuine seams

**Seam 1 — the session-scope injection phase.** Five coupled pieces, all
derived from the same `SessionScope` + downsweep graph, all absent on the
normal path:

1. `depanal excludedVal` — the source-less `Val.G<g>` modules cannot be
   summarised (N passes `[]`).
2. `deferredMods` (target ∪ transitive Val-importers) and the filtered
   `depGraph` handed to `load'` (N loads the full graph).
3. Post-`load'`: restore `hsc_mod_graph = modGraphRaw`, then
   `injectSessionScope` + the `inject` phase emit (N does none of it).
4. Per-module, after `core2core`: `hscTidy` → `mkIfaceTc` → `addToHpt` for a
   deferred module (N never registers anything).
5. `cpSummaries` source: S walks `flattenSCCs (topSortModuleGraph True
   modGraphRaw Nothing)` because (4) makes dependency ORDER load-bearing; N
   takes `mgModSummaries <$> getModuleGraph` post-`load'`.

**Seam 2 — the per-module tier decision, which IS also the loop schedule.**
These are one seam, not two, and the analysis that matters is *why*:

- N's rule (`reachableModuleClosure`) is computed over EVERY module's
  *desugared* Core, so no module's tier is knowable until all desugars have
  run. The loop is therefore forced to STAGE: all fronts, compute the closure,
  then `core2core` the reachable subset.
- S must `core2core` every module (PHASE 3's comment: resolving library calls
  from `load'`-provisioned ifaces bakes kind=4 `ErrorSentinel`s), and its
  seam-1 item (4) requires INTERLEAVING — a later deferred module's
  *typecheck* resolves its `import` from the HPT entry an earlier deferred
  module's `addToHpt` registered, so front(B) must follow back(A).

So: "compile everything" ⟹ interleaved is legal; "tier by global Core
reachability" ⟹ staged is mandatory. One `TierPolicy` value picks both, and
neither pipeline can pick the other half independently. This is why the
schedule is not a third seam — it is the same seam seen from the other side.

Attempted collapse, rejected: registering a deferred module's iface from the
*desugared* (pre-`core2core`) guts would let S stage too. It would also change
what `mkIfaceTc` sees on the single most delicate path in the extractor (the
one item 20(b) and the PHASE 3 comment both live on). Not worth it here; the
`TierPolicy` split costs ~10 lines and buys zero risk.

### Small conventions (parameters, not seams)

| | N | S |
|---|---|---|
| error-message label | `runPipeline` | `runSessionPipeline` |
| bind-type capture occs | `"result" <|> "__result"` | `"__result"` only |
| load barrier position | AFTER the compile loop (deliberate: PASS 1's own `parseModule`/`typecheckModule` re-raises a spanned `SourceError` first; an early check would replace it with a generic message) | BEFORE the graph restore / injection / PHASE 3 (deliberate: never inject into a half-populated HPT) |
| `prHscEnv` | raw `getSession` | `hscUpdateFlags canonicalizeDFlags` |

All four become fields on the plan record. The barrier positions are genuinely
opposite and both are load-bearing, so the skeleton offers two hook points
(`cpAfterLoad`, `cpBeforeMerge`) and each variant fills exactly one.

## Things that must not move

- **Timing wire grammar.** One line per phase per process, phase names
  unchanged: `startup`, `ghc_setup`, `ghc_load`, `inject` (session only),
  `typecheck`, `core`. `tidepool-harness/src/timing.rs` documents this and the
  two modules stay in sync by hand. No phase is renamed, added, removed, or
  re-timed.
- **`e6-tier` diagnostic line.** Same format, same normal-path-only emission,
  same POSITION (after the `typecheck`/`core` phase lines). The unified loop
  returns the reachable set out of the staged branch so the report is still
  emitted at the original point in the stderr stream.
- **`capturedUserType` / `capturedBindingType` capture points** — at typecheck
  time, off the same `tcGblEnv`, before any optimization.
- **`canonicalizeDFlags` application points** — session flags at setup
  (inside `extractionDynFlags`), per-summary `ms_hspp_opts`, and the per-module
  `HscEnv` via `hscUpdateFlags`. Unchanged, now written once.
- **`"runPipeline: module load failed compiling"`** — pinned verbatim by
  `haskell/test-fidelity/Fidelity/PrimopArity.hs:107,144`.

## Two deliberate, flagged behaviour notes

1. **`core` phase rounding on the session path.** S currently times desugar and
   `core2core` inside ONE `timeSection`; the unified front/back split times
   them separately and sums. The phase name, line, and grammar are unchanged;
   only the millisecond value can differ by rounding (≤1ms per module). N is
   bit-identical (it already timed them separately).
2. **`prTyCons` on the session path** now comes from every module's *desugared*
   guts in summary order (N's existing expression) rather than
   `depGuts ++ targetGuts`. These coincide: `core2core` transforms `mg_binds`
   only and never re-derives `mg_tcs` (N's own comment, lines 363-368), and S's
   summaries are `topSortModuleGraph`-ordered over a graph that is the target's
   own import closure — so the target is last and `allGuts \ target ++ target`
   is the summary order. The repl suite (203 tests, session path) is the
   oracle for this.
3. **Target-not-found message** is unified to
   `"<label>: target module '<T>' not found among compiled modules: …"`. N
   previously had no label prefix and S said "not found among:". Nothing in the
   tree matches either string; it is an internal-bug path.

## Resulting shape

```
runCompile :: PipelineVariant -> FilePath -> [FilePath] -> IO PipelineResult
  -- the whole skeleton, including the ONE hs-boot filter, the ONE
  -- canonicalizeDFlags per-module site, the ONE compileFront (parse →
  -- typecheck → capture → desugar) and ONE compileBack (core2core → hook →
  -- externalize).

data PipelineVariant  -- label, downsweep excludes, and a plan builder
data CompilePlan      -- the seam values, computed from the downsweep graph
data TierPolicy = OptimizeEveryModule | OptimizeCoreReachable

normalVariant  path       -- OptimizeCoreReachable + no injection
sessionVariant scope path -- OptimizeEveryModule  + the five injection pieces
```
