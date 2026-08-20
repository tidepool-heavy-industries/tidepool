# Turn latency: state injection for the outer render/loop compile

**Status: designed, not started.** Narrow-first per operator decision
(2026-08-20): prove the mechanism on the one compile that dominates turn wall
time; generalize the memo keying later if it earns it.

## Problem

Run-4 phase attribution: ~67% of a companion turn's wall time is the fused
outer `render`/`loop` extract compile (~6 min), re-paid EVERY turn. Cause: the
driver splices the checkpointed state into the module as a source literal —

```haskell
__selfHarnessState = case Aeson.eitherDecode "{...state json...}" of ...
__operatorMsg = Just "..."   -- operator steering, same problem
```

— so each turn's outer module is textually new and the content-addressed
compile memo (`tidepool_runtime::cache`) misses by construction.

## Why injection is sound (verified 2026-08-20)

The session inject plane (`--inject-val`) works via **thin `.hi` ifaces**: the
compile consumes only a NAME + TYPE; the value is a heap object resolved by
`stableVarId` at runtime. The compiled artifact is value-independent. The memo
currently refuses session compiles for two reasons that do NOT apply to a
consume-only outer compile: it writes no session iface (no on-disk side
effect), and its injected iface can be a FIXED module with turn-invariant
content (the mutable-state concern was about generation-numbered
`Val.G<g>` ifaces that change under you).

## Design

1. **Inject the JSON as `Text`, keep the typed decode in Haskell.** A stable
   val module (fixed name, e.g. `Tidepool.Session.Val.HarnessCtx`) exposing
   `__stateJson :: Text` and `__operatorMsgJson :: Text`. The template becomes
   turn-invariant:

   ```haskell
   __selfHarnessState = case Aeson.eitherDecode __stateJson of
     Right s -> s
     Left _  -> Loaded.initialState   -- decode-failure fallback stays typed, in-language
   ```

   Fresh boot injects `"null"` → `initialState`; the driver's existing
   StateDecode carry-forward/fallback ladder keeps its observable behavior
   (assert in tests, adjust which layer does the falling back as needed).
   Runtime constructs only `Text` heap values — no bridge for `Loaded.State`
   needed, and the state SHAPE still typechecks against the harness's own
   `State` (the decode is compiled, only the payload moves).

2. **Memo learns exactly this shape.** `ExtractCmd` argv allowlist classifies
   `--inject-val <the stable module>` as cacheable; the stable iface is keyed
   by CONTENT like any include root. Everything else about
   `invocation_key` unchanged. Generation-numbered `Val.G<g>` injections stay
   uncacheable exactly as today.

3. **Driver plumbing.** Per turn: write/refresh the stable iface (once per
   harness-source version), tenure the two `Text` values under the module's
   var_ids, compile via the (now-hitting) memoized path, run.

4. **Variable-splice inventory** (the memo misses if ANY literal remains):
   `__selfHarnessState`, `__operatorMsg` are the known two; the lane must
   diff two consecutive turns' rendered outer modules and drive the diff to
   zero as its core acceptance.

## Acceptance

- Two consecutive turns with different state/steering: rendered outer module
  byte-identical; second turn's outer compile is a memo HIT (0 extract
  spawns); state and steering observed correctly by the run.
- Decode-failure fallback: a shape-incompatible injected state lands on
  `initialState` with today's observable retry behavior.
- Cold correctness unchanged; non-companion paths byte-identical.

## Non-goals (this pass)

- General memo coverage for `Val.G<g>` session binds.
- Any change to checkpoint format or the harness `State` type.

## Measured attribution (compile-attribution lane, 2026-08-20)

Cold fused companion compile, 51 modules, 32.7s total (proportions
generalize to the live ~361s; absolute seconds don't — bigger harness,
colder box): `ghc_load` upsweep 59.6%, attributed typecheck+core 35.9%.
Within the attributed pass: 49 byte-identical stdlib modules = 68.6%,
the harness's own two modules = 31.4%. **~2/3 of all compile wall goes to
modules identical across every compile anywhere.** And 10 of 12 real
spawns in one companion run are per-node `--turn` compiles (29-38 modules,
zero own content) that state-injection can never help — each node's code
differs by construction.

Consequences for sequencing:
- **State injection stands** (it takes the fused outer compile to a full
  memo hit — zero spawns — which no module cache can), but it is the
  smaller of the two wins.
- **The persistent build-products dir helps every compile including the
  dominant `--turn` class** — see the "Build-products dir: as-built" section
  below for the finished spike, the mechanism that shipped, and the
  determinism gap that keeps it opt-in rather than default-on.

## Build-products dir: as-built (2026-08-20, build-products-dir lane)

**Spike (prerequisite 2): GREEN, verified via the real GHC API.**
`haskell/spike-build-products/Spike.hs` (`cabal test spike-build-products`)
drives `load'` directly, three cycles, each its OWN fresh `runGhc` session
(the closest same-process proxy for "a fresh `tidepool-extract` process"),
instrumented with a `Messager` that records GHC's own per-module
`RecompileRequired` verdict:

```
cycle 1 (cold, bpDir empty):        14/14 modules NeedsRecompile, load' 702ms
cycle 2 (warm, Target.hs UNCHANGED): 0/14 modules NeedsRecompile, load' 256ms
cycle 3 (warm, Target.hs CHANGED):   1/14 modules NeedsRecompile (Target only), load' 265ms
```

`checkOldIface`, under `backend = noBackend` (what `canonicalizeDFlags` pins
every extraction to), DOES skip an unchanged home module across independent
GHC sessions when interfaces are written to (`-fwrite-interface`) and read
back from a stable `hiDir`/`objectDir`. A genuine content edit still forces
exactly that module to recompile while every stdlib dependency stays
skipped — the production shape (a novel per-turn module, a fixed stdlib
closure).

**Follow-up: "teach the manual second pass to reuse `load''`s results"
(prerequisite 3) is BLOCKED, not just undone.** `GhcPipeline.hs`'s manual
per-module loop (`compileFront`/`compileBack`) needs actual `CoreBind`s for
translation — `parseModule`/`typecheckModule` always do full frontend work
regardless of what `load'` decided, so a warm `load'` alone doesn't shrink
that loop. The obvious fix — reconstruct a skipped module's Core from its
loaded interface's unfoldings (`Opt_ExposeAllUnfoldings` is already on) —
was tried and measured directly: comparing `Tidepool.Prelude`'s own
top-level binder set from a fresh compile against the same module
skip-loaded from a warm bpDir (`GHC.getModuleInfo` / `modInfoTyThings` /
`maybeUnfoldingTemplate` on each `Id`'s `idUnfolding`), **only 6 of 133
top-level binders (4.5%) had a reconstructable unfolding** — GHC only
writes unfoldings for bindings its own heuristics judge worth exposing, not
literally every binding's full body. See `runCoreReuseFollowUp` in the same
spike file for the reproducible probe. Closing this needs GHC's real
`typecheckIface`/interface-hydration machinery, a materially different (and
riskier) mechanism, not attempted here.

**Shipped: the plumbing, end to end, gated OFF by default.**
- `tidepool-runtime/src/paths.rs::build_products_dir(fingerprint)` —
  `$TIDEPOOL_BUILD_PRODUCTS_DIR` override, else content-addressed under
  `compile_cache_dir()/build-products/<fingerprint>`, where `fingerprint` is
  the resolved extract binary's own content fingerprint
  (`toolchain::extract_fingerprint`, already memoized). Deliberately keyed on
  the EXTRACT BINARY alone, not "extract + stdlib" as originally scoped: a
  stdlib edit is already safely handled per-module by GHC's own interface
  content-hash (spike cycle 3 proves this directly), so folding stdlib
  content into the directory's own identity would cost a full stdlib-tree
  walk per compile for a property GHC already gives for free. Fresh-dir-on
  -toolchain-change still makes staleness structurally impossible.
- `tidepool-extract-cmd`'s `ExtractCmd::build_products_dir` — the
  `--build-products-dir <dir>` flag, and `app/Main.hs` parses it into
  `argBuildProductsDir`, setting `$TIDEPOOL_BUILD_PRODUCTS_DIR` once at
  startup before any `GhcPipeline` call.
- `Tidepool.GhcPipeline.withBuildProductsFromEnv` reads that env var (mirrors
  `getLibdir`'s own `$TIDEPOOL_GHC_LIBDIR` pattern) and sets
  `hiDir`/`objectDir` + `Opt_WriteInterface`, applied at both `setSessionDynFlags`
  call sites (`runCompile` and `runBatchPipeline`) — a no-op, byte-identical
  `DynFlags`, when unset.
- `tidepool_runtime::cache::invocation_key` allowlists `--build-products-dir`
  as DROPPED (same bucket as `--output-dir`) — a directory whose location
  never changes the output bytes, only how much frontend work GHC redoes to
  produce them; see `invocation_key_drops_build_products_dir`.

**NOT activated by default — a real determinism gap, found by the mandatory
differential test.** `tidepool-runtime/src/artifacts.rs::compile_invocation`
only wires `.build_products_dir(...)` onto the spawned `ExtractCmd` when the
CALLER has already set `$TIDEPOOL_BUILD_PRODUCTS_DIR` — presence of the env
var is both the location override and the enable switch, so a normal compile
today is 100% unaffected (re-verified: two independent cold compiles of the
same source, no build-products dir at all, are still byte-for-byte identical
— the pre-lane baseline).

Turning it ON exposed a real bug: a cold compile and a warm (build-products
dir) compile of BYTE-IDENTICAL source produced DIFFERENT output. A
structural CBOR diff (node-for-node comparison, not a byte diff) showed the
tree SHAPE and node COUNT were identical between the two runs; only `VarId`
values at Case-binder positions differed. Root cause: `Tidepool.Translate`'s
`localVarId` (used for every NESTED, non-top-level `Id` — lambda parameters,
case scrutinee/alt binders, local lets) hashes GHC's raw session `Unique`
directly (`occ ++ "#" ++ show (getKey (varUnique v))`). That raw Unique's
VALUE depends on how many uniques earlier work in the SAME GHC session
already consumed — harmless before this lane (every extraction did IDENTICAL
`load'` work, so the Unique baseline reaching any given module's own compile
was always the same for the same source+includes+flags), but NOT harmless
once `load'` can skip a variable number of modules: a cold and a warm
session consume different quantities of uniques before reaching the SAME
module's compile, baking a different suffix into the same logical local
binder.

A fix was attempted at `GhcPipeline.hs::externalizeInternalTops` (the #313
fix, which bakes a similar raw-Unique suffix, but only for TOP-LEVEL
binders) — replacing its raw Unique with a deterministic per-module
first-occurrence index. It did NOT close the gap (confirmed: same failing
byte offset before and after), because `externalizeInternalTops` never
touches NESTED binders at all — the leak is squarely `localVarId`, a
different function, used throughout the whole Core tree. The revert is
clean (`externalizeInternalTops`'s body is byte-for-byte its original text;
`git diff` on `GhcPipeline.hs` shows no change inside that function). A real
fix needs a stable, content-derived numbering scheme for NESTED Ids across
the whole `Translate.hs` pipeline — a separate, higher-risk lane, not
attempted here.

`tidepool-runtime/tests/build_products_dir_differential.rs` pins this
finding as an `#[ignore]`d reproduction (registered in
`.config/watched-tests.toml`) — the mandatory differential test this
boundary calls for exists and correctly documents the gap, but is not
expected to pass until the `localVarId` fix lands. Whoever picks that up:
`cargo nextest run --ignore-default-filter -p tidepool-runtime -E
'binary(build_products_dir_differential)' --run-ignored ignored-only`
reproduces it directly.

**Net effect of this lane:** zero behavior change for every existing caller
(nothing sets `$TIDEPOOL_BUILD_PRODUCTS_DIR` today); a fully-built,
unit-tested on-ramp (location resolver, CLI flag, memo classification, the
`GhcPipeline.hs` seam) ready for the NEXT lane to flip on the moment the
`localVarId` determinism gap closes; and a concretely bounded remaining
task (fix one function's identifier scheme) rather than an open-ended one.

The default-on per-compile summary line (`compile summary modules=…
wall_ms=… top=…`) now lands in harness logs at INFO from both the
artifacts and session `--turn` lanes; full per-module tables stay behind
`TIDEPOOL_TIMING`.

## Direction: toward a resident compile daemon (operator decision 2026-08-20)

The eventual form is a resident extract daemon (the ghcide/HLS convergence;
in-repo precedent: `tidepool-lsp-daemon`). A single daemon IS viable despite
the forking session tree, because tidepool already externalized the one thing
GHCi keeps ambient: there is no mutable interactive context — every compile's
scope arrives as explicit request data (import lists, injected iface names,
include roots, all in the ONE `ExtractCmd` builder). A daemon therefore holds
only (a) warm compiler state and (b) an append-only store of immutable
compiled modules — shareable across all branches/windows by construction.
Daemon-readiness rules to preserve meanwhile:
- Every compile input stays EXPLICIT in the invocation (no ambient state
  creep) — the memo allowlist already enforces this; keep it strict.
- All spawns keep routing through `tidepool-extract-cmd` — the daemon swap
  is then one seam.
- A persistent shared build-products dir (module-granular GHC recompilation
  avoidance across spawns — built and spike-verified, see "Build-products
  dir: as-built" above) is ALSO the daemon's disk-backed seed; growth is
  bounded by retirement/rotation at quiescent points, mirroring machine
  rotation. **A daemon is MORE exposed to the `localVarId` determinism gap
  documented above, not less**: a long-running process's session-Unique
  counter drifts continuously as it serves turn after turn, so two
  back-to-back compiles of otherwise-identical source inside one daemon
  process would ALREADY diverge on nested-Id `VarId`s today, warm
  build-products dir or not — daemon mode cannot land before that gap
  closes, independent of whether this dir is in play.
