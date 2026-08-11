# batch-turns spike findings: §2.3, the session-reuse gate

**Lane:** `batch-turns`. Spike-only landing per
`plans/post-restart/batch-turns-feasibility.md` §2.3 — this document does not
touch that file (parent-owned) and does not re-derive (a)/(b)/(c), which that
document already answered.

**Spike:** `haskell/spike-batch/Spike.hs`, `test-suite spike-batch` in
`haskell/tidepool-extract.cabal`. Run: `cabal test spike-batch` (needs the
with-packages GHC on `PATH`, per `haskell/CLAUDE.md`). No file outside
`haskell/spike-batch/`, `haskell/tidepool-extract.cabal`, `.gitignore`, and
this findings doc was touched — `GhcPipeline.hs`/`Session.hs`/`app/Main.hs`/
any Rust file are unmodified; the mechanism was lifted (copied), not imported,
because `runCompile`'s two un-exported helpers (`canonicalizeDFlags`,
`capturedBindingType`) and the whole `sessionVariant` compile-loop skeleton
are not part of `GhcPipeline`'s export list.

## The question

> Can a SINGLE `runGhc` session run N sequential
> `setTargets`/`depanal`/`load'` cycles with items 2..N skipping the stdlib
> `load'`?

Answered by a direct observation, not a timing inference: a caller-supplied
`Messager` passed to `load'` records GHC's OWN per-module recompile verdict
(`UpToDate` vs `NeedsRecompile <reason>`) for every module `load'` visits.

## The mechanism built

One `runGhc` session per scenario, three sequential cycles. Each cycle:

1. `setTargets [InputK.hs]` (imports `Tidepool.Prelude` — the real stdlib
   closure: 12 home-package modules — for genuine stdlib compile cost).
2. `depanal excludedVal False` — `excludedVal` = the Val session modules
   already injected by prior cycles (source-less, so excluded from the
   downsweep exactly as `sessionVariant` excludes them via
   `pvDownsweepExcludes`).
3. The target-deferral closure (`deferredMods`) and `depGraph` filter,
   copied verbatim from `sessionVariant` (GhcPipeline.hs:592-629). With one
   module per cycle this closes to just `{target}`.
4. `load' <cache> LoadAllTargets mkUnknownDiagnostic (Just <our Messager>)
   (mapMG unpoison depGraph)` — the instrument. `<cache>` is the ONE thing
   varied between the two scenarios below.
5. Restore `hsc_mod_graph = modGraphRaw`, then `injectSessionScope` (the
   PRODUCTION, unmodified `Tidepool.Session` function) to inject every prior
   cycle's Val iface into the HPT.
6. The interleaved per-module compile loop (`OptimizeEveryModule` tier,
   copied from `runCompile`'s `compileFront`/`compileBack`): parse,
   typecheck, capture `__result`'s type via a copied `capturedBindingType`,
   desugar, `core2core`, and — for the deferred target only —
   `hscTidy`+`mkIfaceTc`+`addToHpt` register it into the HPT with
   `emptyHomeModInfoLinkable` (risk 1's exact mechanism).
7. `stripMonadHead` (PRODUCTION, exported, unmodified) on the captured type,
   `mkThinSessionIface`/`writeSessionIface` (PRODUCTION, unmodified) mint and
   write `Tidepool.Session.Val.G<k>` carrying binder `v :: Text`.

Chain: `Input2.hs` does `import Tidepool.Session.Val.G1 (v)` and uses `v` in
its own `__result`; `Input3.hs` does the same against `G2`. This is the
load-bearing reference risk 2/3 worry about — proof (or disproof) that a
LATER cycle's typecheck resolves an EARLIER cycle's injected binder, in the
same session, is the `resolvedPriorBinder` column below.

## The measured table (verbatim, `cabal test spike-batch`)

Two scenarios, each its own complete `runGhc`, 3 cycles:

- **Scenario A** — `load' Nothing ...`, byte-for-byte what every `load'`
  call site in this codebase (including `GhcPipeline.hs:256`) passes today.
  This is the direct answer to §2.3 as posed.
- **Scenario B** — the one variation this spike tried (see below): a live
  `GHC.Driver.Make.ModIfaceCache`, created once via `newIfaceCache`, threaded
  as `load'`'s first argument on every cycle instead of `Nothing`.

```
################ SCENARIO: A: no ModIfaceCache (matches production `load' Nothing ...`) ################

--- cycle 1  target=Input1  resolvedPriorBinder=Nothing ---
  Tidepool.Aeson.Scientific                  NeedsRecompile(MustCompile)
  Tidepool.Data.Text                         NeedsRecompile(MustCompile)
  Tidepool.Aeson.Value                       NeedsRecompile(MustCompile)
  Tidepool.Aeson.Lens                        NeedsRecompile(MustCompile)
  Tidepool.Aeson.FromJSON                    NeedsRecompile(MustCompile)
  Tidepool.Aeson                             NeedsRecompile(MustCompile)
  Tidepool.Data.Time                         NeedsRecompile(MustCompile)
  Tidepool.FilePath                          NeedsRecompile(MustCompile)
  Tidepool.QQ.Fmt.Runtime                    NeedsRecompile(MustCompile)
  Tidepool.Records.Bridged                   NeedsRecompile(MustCompile)
  Tidepool.Records                           NeedsRecompile(MustCompile)
  Tidepool.Prelude                           NeedsRecompile(MustCompile)
  load' wall-clock:        566 ms
  compile-loop wall-clock: 2148 ms

--- cycle 2  target=Input2  resolvedPriorBinder=Just True ---
  Tidepool.Aeson.Scientific                  NeedsRecompile(MustCompile)
  Tidepool.Data.Text                         NeedsRecompile(MustCompile)
  Tidepool.Aeson.Value                       NeedsRecompile(MustCompile)
  Tidepool.Aeson.Lens                        NeedsRecompile(MustCompile)
  Tidepool.Aeson.FromJSON                    NeedsRecompile(MustCompile)
  Tidepool.Aeson                             NeedsRecompile(MustCompile)
  Tidepool.Data.Time                         NeedsRecompile(MustCompile)
  Tidepool.FilePath                          NeedsRecompile(MustCompile)
  Tidepool.QQ.Fmt.Runtime                    NeedsRecompile(MustCompile)
  Tidepool.Records.Bridged                   NeedsRecompile(MustCompile)
  Tidepool.Records                           NeedsRecompile(MustCompile)
  Tidepool.Prelude                           NeedsRecompile(MustCompile)
  load' wall-clock:        145 ms
  compile-loop wall-clock: 2050 ms

--- cycle 3  target=Input3  resolvedPriorBinder=Just True ---
  [identical: 12/12 NeedsRecompile(MustCompile)]
  load' wall-clock:        134 ms
  compile-loop wall-clock: 2051 ms

VERDICT:
  cycle 1: 12/12 load' entries NeedsRecompile
  cycle 2: 12/12 load' entries NeedsRecompile
  cycle 3: 12/12 load' entries NeedsRecompile
  RED

################ SCENARIO: B: WITH a live ModIfaceCache threaded across all 3 cycles ################

--- cycle 1  target=Input1  resolvedPriorBinder=Nothing ---
  [12/12 NeedsRecompile(MustCompile) — cold, cache starts empty]
  load' wall-clock:        418 ms
  compile-loop wall-clock: 2195 ms

--- cycle 2  target=Input2  resolvedPriorBinder=Just True ---
  Tidepool.Aeson.Scientific                  UpToDate
  Tidepool.Data.Text                         UpToDate
  Tidepool.Aeson.Value                       UpToDate
  Tidepool.Aeson.Lens                        UpToDate
  Tidepool.Aeson.FromJSON                    UpToDate
  Tidepool.Aeson                             UpToDate
  Tidepool.Data.Time                         UpToDate
  Tidepool.FilePath                          UpToDate
  Tidepool.QQ.Fmt.Runtime                    UpToDate
  Tidepool.Records.Bridged                   UpToDate
  Tidepool.Records                           UpToDate
  Tidepool.Prelude                           UpToDate
  load' wall-clock:        1 ms
  compile-loop wall-clock: 1968 ms

--- cycle 3  target=Input3  resolvedPriorBinder=Just True ---
  [12/12 UpToDate]
  load' wall-clock:        1 ms
  compile-loop wall-clock: 2040 ms

VERDICT:
  cycle 1: 12/12 load' entries NeedsRecompile
  cycle 2: 0/12 load' entries NeedsRecompile
  cycle 3: 0/12 load' entries NeedsRecompile
  GREEN
```

(Full untruncated output, all three cycles' individual module lists included,
is reproducible verbatim by running `cabal test spike-batch`; the cycle-3
block is elided above only because it is byte-identical in shape to cycle 2.)

## Resolution: does item k+1 typecheck resolve item k's injected binder?

**Yes, in both scenarios, all cycles.** `resolvedPriorBinder=Just True` for
cycle 2 (resolving `G1.v`) and cycle 3 (resolving `G2.v`) in BOTH Scenario A
and Scenario B. The `mkThinSessionIface`/`writeSessionIface`/
`injectSessionScope` mechanism (already production code, exercised here
across a 3-deep chain in one continuing session rather than session-c-test's
single injection) is not the blocked part of this design — it is exactly as
sound mid-session as it is per-spawn. This confirms (a)'s "passes on the type
plane by construction" conclusion holds under repetition, not just once.

## VERDICT

**RED, as posed** — but with a named, minimal-diff fix candidate that flips
it GREEN.

**Scenario A (production-faithful — `load' Nothing ...`): RED.** Every one of
the 12 stdlib home-package modules in `Tidepool.Prelude`'s import closure
reports `NeedsRecompile(MustCompile)` from `load'`'s own `Messager`, on
EVERY cycle, including cycles 2 and 3 where those exact modules are already
sitting in the session's HPT (compiled by cycle 1's own `load'`). GHC's own
stated reason is `MustCompile` — not a fingerprint/flag-mismatch reason, the
"no old interface available" class.

**The blocking mechanism, named precisely.** `load'`
(`GHC.Driver.Make.load'`, `compiler/GHC/Driver/Make.hs` in the GHC 9.12.2
source tree) unconditionally clears the session's home-package table at the
top of EVERY invocation:

```haskell
    let pruneHomeUnitEnv hme = hme { homeUnitEnv_hpt = emptyHomePackageTable }
    setSession $ discardIC $ hscUpdateHUG (unitEnv_map pruneHomeUnitEnv) hsc_env
    hsc_env <- getSession
    liftIO $ unload interp hsc_env
```

and then reconstructs its recompile-avoidance baseline (`old_hpt`, fed to
`upsweep`) purely from `iface_clearCache` on the CALLER-SUPPLIED
`Maybe ModIfaceCache` argument — never from the live `hsc_HPT` an earlier
`load'` call (or our own `cpAfterModule`-style manual HPT registration) left
behind:

```haskell
    cache <- liftIO $ maybe (return []) iface_clearCache mhmi_cache
    let !pruned_cache = pruneCache cache (...)
    ...
    liftIO $ upsweep worker_limit hsc_env mhmi_cache diag_wrapper mHscMessage
               (toCache pruned_cache) build_plan
```

`load' Nothing ...` — the call shape used everywhere in this codebase today
(`GhcPipeline.hs:256`, and this spike's Scenario A) — supplies
`mhmi_cache = Nothing`, so `cache = []` on every single call, so `old_hpt =
mempty` every time, so EVERY module `load'` visits looks brand new to
`checkOldIface` regardless of what the live session's HPT actually holds.
This is not an ambiguity in how `load'` reads "in HPT, no linkable" (risk 1's
original framing) — it is blunter: `load'` never consults the live HPT for
recompile avoidance at all. It deletes it and starts over, every call, by
design (the `-- write an empty HPT to allow the old HPT to be GC'd` comment
at the deletion site is GHC's own stated rationale).

## Variation tried (the one the boundary invites)

**Thread a live `ModIfaceCache` across cycles instead of `Nothing`.**
`GHC.Driver.Make` exports exactly the persistence mechanism `load'` is
designed to consult: `newIfaceCache :: IO ModIfaceCache`, created once
(outside any cycle), passed as `Just cache` on every `load'` call. Every
module `upsweep` finishes gets pushed into it (`addHmiToCache`,
`Make.hs:1157`) as a side effect of compiling; the NEXT `load'` call drains it
via `iface_clearCache` and uses its contents as `old_hpt`.

**Result: flips the verdict to GREEN.** Scenario B, byte-identical spike
mechanism otherwise, cycles 2 and 3 report `UpToDate` for all 12 stdlib
modules; `load'` wall-clock collapses from 145-566 ms (Scenario A, all
cycles) to 1 ms (Scenario B, cycles 2-3). Binder resolution
(`resolvedPriorBinder`) is unaffected — still `Just True` throughout — so the
fix does not trade away the type-plane mechanism (a) already proved sound.

**This is a real fix candidate, not a workaround that weakens the probe**:
`ModIfaceCache` is GHC's own documented API for exactly this use case (see
`Note [Caching HomeModInfo]` at `Make.hs:411-431`, written for "API clients
who call `load` like to cache the HomeModInfo in memory between calls to this
function" — i.e. precisely a batched, multi-`load'`-per-session caller like
this lane). No monkey-patching, no relying on `load'` internals beyond its
own public parameter.

## The caveat this variation does NOT remove

Even under Scenario B (`UpToDate` + 1 ms `load'`), the interleaved
per-module compile loop — parse/typecheck/desugar/`core2core` over the WHOLE
stdlib closure PLUS the target, run unconditionally every cycle regardless of
`load'`'s verdict, per `sessionVariant`'s own documented rationale
(`GhcPipeline.hs:702-715`: HPT-provisioned ifaces carry no -O2 unfoldings, so
library calls must be resolved from freshly-recompiled bodies, not
`load'`-provisioned interfaces) — stays flat at ~2.0-2.2 s per cycle in BOTH
scenarios. `load'` amortization recovers only the 145-566 ms slice of a
~2.6-2.7 s total per-cycle GHC cost (roughly 5-20%), not the dominant cost.

This is NOT a contradiction of the feasibility doc — §1 already scopes the
win to "amortization of GHC boot + stdlib `load'`... not of per-item
typecheck" and explicitly separates the two. It is a sizing correction worth
carrying into the next step: GHC boot itself (session init, paid once
regardless of scenario) and `load'` (fixable per Scenario B) are both real,
recoverable costs; the interleaved compile loop is not, and it is the larger
number.

## One-line verdict

**RED as posed (production's `load' Nothing ...` recompiles the entire
stdlib closure every cycle, GHC's own stated reason `MustCompile`, because
`load'` unconditionally clears the session HPT and only accepts
recompile-avoidance state through an explicit `ModIfaceCache` nothing in this
codebase currently supplies) — but GREEN under a named, minimal, in-tree-API
fix (thread a `ModIfaceCache` across cycles instead of `Nothing`), with the
binder-injection chain (a) already proved intact under that fix and the
caveat that the recoverable win is bounded by `load'`'s own share of a
cycle's cost (~5-20%), not the full per-cycle GHC-side cost.**
