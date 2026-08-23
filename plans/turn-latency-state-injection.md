# Turn latency: state injection for the outer render/loop compile

**Status: landed (2026-08-22).** Narrow-first per operator decision
(2026-08-20): prove the mechanism on the one compile that dominates turn wall
time; generalize the memo keying later if it earns it. The fused outer
render/loop compile's SOURCE is now turn-invariant, and the compile memo
verifiably HITS it from the second turn onward — see "Landed" below for what
shipped, the one design delta found only during testing, and the receipts.

## Drift note (implementation start, 2026-08-22)

Deltas found between this doc and the current code, before any behavior
change landed:

1. **The outer render/loop compile has zero session-plane wiring today.**
   `SelfHarnessDriver::compile_cycle_entry` calls `engine::compile_turns` →
   `tidepool_runtime::artifacts::compile_targets`, which never sets
   `--session-root`/`--inject-val`/`--session-bind` on its `ExtractCmd` — the
   state/operator-msg splice is pure source text, exactly as the Problem
   section says, but there was no prior partial wiring to build on.
2. **`--session-root` alone reroutes `tidepool-extract-bin`'s dispatch away
   from the multi-target path.** `Main.hs`'s `main` sends `isSessionMode args
   -> processSessionFile` ahead of the plain `processFile` (multi-target)
   branch, and `processSessionFile` only ever compiles ONE target
   (`argTarget`, defaulting to `__result`) — it has no `--targets` handling
   at all. The fused compile needs BOTH targets (`result` +
   `__selfHarnessLoopEntry`) in one spawn, so turning on `--session-root`
   unmodified would silently drop the loop entry. Fix (in scope, not a new
   crate boundary): reorder `main`'s guards so `not (null argTargets)` wins
   over `isSessionMode`, and make `processFile` itself session-scope-aware
   (`runPipelineSession (if isSessionMode args then Just (scopeFromArgs args)
   else Nothing)` in place of the unconditional `runPipeline`). Verified safe
   for every existing non-session multi-target caller: `runPipelineSession
   Nothing`/an inert scope is byte-identical to `normalVariant`
   (`runPipelineSession`'s own doc), and no existing caller sets
   `--session-root` alongside `--targets`.
3. **The value-plane "tenure a session Val binding" mechanism already exists
   end to end** (`tidepool_runtime::session::turn::compile_session_turn`'s
   `--session-bind` path + `ResidentSession::run_bind`, which compiles, runs,
   tenures, AND registers the `BindingTable` entry in one call) — no new
   Rust/Haskell machinery needed for that half. The one gap: the harness
   driver's outer session (`OuterSession { sid, .. }`) is a bare `SessionId`
   today with no prior value-plane use, so `run_bind` had never been called
   against it.
4. **Reusing `Tidepool.Session.Val.G<g>`'s existing naming scheme at a
   reserved, non-rotating `Generation(0)`** — rather than inventing a new
   module-kind/name shape (the plan's illustrative
   `Tidepool.Session.Val.HarnessCtx`) — turned out sufficient and needs zero
   Haskell-side naming changes: `PersistentSession::new` starts `val_gen` at
   `Generation(0)` ("the empty session — no Lib/Val module exists yet"), a
   real bind always mints `val_gen().next()` (>= 1), and `set_val_gen`'s
   monotonic-max rule means rebinding at a passed-in `gen: Generation(0)`
   every cycle never advances the counter — so gen 0 can never collide with a
   real future bind on this session. One module (`Tidepool.Session.Val.G0`)
   carries BOTH crossings as a single `(Text, Text)` tuple binder
   (`__harnessCtx`), rather than two separately-bound names, since
   `ResidentSession` exposes only the single-binder `run_bind` (no
   multi-binder/projected wrapper) and adding one was judged out of the
   narrow scope this pass is locked to.
5. **Scope stays narrow at the driver layer too**: only
   `compile_cycle_entry` (the production fused path `run_one_cycle` actually
   calls) gets the injection treatment. The older unfused paths
   (`compile_outer`/`render_framing`, and `run_loop_fragment_inner`'s
   `precompiled: None` branch — doc'd as "a direct fragment API a test drives
   in isolation, never called from `run_one_cycle`") keep the original
   literal-splice `state_cross::state_in`/`operator_msg_in` unchanged; new
   sibling functions carry the injected-Text shape for the one path in scope.
6. **The harness-ctx refresh's own compile is intentionally never
   memo-cacheable** — a `--session-bind` invocation with fresh `(Text, Text)`
   literal content every cycle is hazard (b) in `plans/compile-memo.md` by
   construction. It is the plan's own "smaller of the two wins" cost: a
   two-import (`Data.Text` only) tuple-literal compile, orders of magnitude
   cheaper than the ~51-module fused outer module it unblocks.

## Landed (2026-08-22)

Every piece above shipped as designed, plus one delta the drift note above
didn't anticipate — found only by actually running the two-cycle acceptance
test, not by reading:

7. **The harness-ctx bind's `--session-root` MUST be the same directory the
   rest of the OUTER session already uses (`SelfHarnessDriver::outer_plane_root`),
   not a separate directory.** A first attempt gave the harness-ctx bind its
   own `selfharness/harness-ctx` dir. That broke the ANSWERER's own turn
   compile with `injectSessionIface: readIface failed for
   Tidepool.Session.Val.G0` — because the outer session is ONE
   `PersistentSession`/`BindingTable` (the one-session collapse), so the
   moment `Val.G0` is registered there, EVERY later compile on that session
   that consults `live_val_modules`/`current_val_modules` (in particular an
   answerer turn's compile, which injects every live session value) reports
   `Val.G0` as something IT should inject too — resolving the iface from
   WHATEVER session_root THAT caller uses (`outer_plane_root`, the decl
   plane's root), not a directory only the harness-ctx refresh knew about.
   Fixed by having `SelfHarnessDriver::harness_ctx_session_root` delegate to
   `outer_plane_root` outright: one root per session, matching every other
   consumer of this session's value plane. Safe to share — `open_outer_plane`
   wipes that dir only at `bootstrap` (once per session; a machine ROTATION
   transfers the existing `SessionLib` rather than re-wiping), and the
   harness-ctx iface's content is a pure function of a fixed name/type, so
   overwriting it in place every cycle alongside the decl plane's own
   `Lib.G<g>.hs` files is harmless.

**What shipped:**
- `tidepool-repr::session_ids::SessionModule::relative_hi_path` — the `.hi`
  path convention the memo fingerprints from.
- `tidepool_runtime::cache::Invocation::stable_val` + `invocation_key`'s
  allowlist widening — accepts `--session-root`/one matching `--inject-val`
  as cacheable, content-fingerprinting the iface file; every OTHER
  `--inject-val` value stays uncacheable exactly as before. Adversarial tests
  in `cache.rs`: path-independence, content-sensitivity, and the
  still-refused non-stable-module case.
- `tidepool_runtime::artifacts::{StableValInject, compile_targets_with_stable_inject}`
  and `tidepool_harness::engine::compile_turns_with_stable_inject` — the
  plumbing from `ExtractCmd` through the memo key, additive (every existing
  caller passes `stable_val: None`, byte-for-byte unchanged).
- `haskell/app/Main.hs`: `processFile` is now session-scope-aware
  (`scopeFromArgs`, shared with `processSessionFile`/`runTurnMode` — three
  copies collapsed to one), and `main`'s dispatch checks `--targets`
  non-empty before `isSessionMode`, so a multi-target + session-scoped
  invocation reaches the multi-target path instead of the single-target
  `processSessionFile`.
- `state_cross::{harness_ctx_module, HARNESS_CTX_BINDING, harness_ctx_source,
  state_in_via_ctx, operator_msg_in_via_ctx}` — the injected-Text siblings of
  `state_in`/`operator_msg_in`. The originals are UNCHANGED and still used by
  the unfused test-only paths (`compile_outer`/`render_framing`,
  `run_loop_fragment_inner`'s `precompiled: None` branch) — narrow scope,
  per drift-note item 5.
- `SelfHarnessDriver::refresh_harness_ctx` — compiles the tiny `__result ::
  Eff '[] (Text, Text)` module (the `Eff` wrapper is load-bearing: a bare
  non-`Eff` value doesn't satisfy the suspendable-binding JIT calling
  convention `run_bind` drives it through) and registers it via `run_bind`,
  called at the top of `compile_cycle_entry` every cycle, before the fused
  compile.
- `tests/state_injection_memo_hit.rs` — the acceptance test: two real cycles
  of the reference harness with genuinely different `State`, asserting (a)
  the `Event::OuterCompile{label:"render+loop"}` source is byte-identical
  across both, and (b) cycle 2 pays strictly fewer `tidepool-extract` spawns
  than cycle 1 (a pinned private memo dir, `support::isolate_compile_memo`).
  Not an exact spawn-count delta: the answerer's OWN turn-compile path
  (`EngineConfig::turn_target`'s standalone shim probe) is independently
  memoized and ALSO warms up between cycle 1 and cycle 2, compounding with
  the fused compile's own hit — confirmed directly via temporary
  instrumentation while developing this test (cycle 1's fused compile: 1
  spawn, MISS; cycle 2's: 0, HIT) and removed before landing.
- `tests/acceptance_boot_compile_count.rs`: `PRE_MODEL_EXTRACT_COMPILES`
  updated 1 → 2 — the harness-ctx refresh adds one small pre-model spawn
  every cycle (including the first), the accepted cost for turning the
  dominant fused compile into a memo hit from cycle 2 onward.

**Verification:** `cargo check --workspace` clean; `cargo fmt --all --check`
clean; fast tier (2049 tests) green; all 5 documented `tidepool-harness`
battery shards green (150 tests); `tidepool-extract-cmd`'s full suite green;
targeted `tidepool-runtime` legs (cache/session/scope/extract-spawn-count)
green except one PRE-EXISTING, unrelated failure
(`cache_tests::test_corrupted_cache_recovery`, confirmed via `git stash` to
fail identically on the clean branch tip — the eval cache dir now also holds
the materialized stdlib as a subdirectory, and the test's cleanup loop
assumes every top-level entry is a file). `cargo clippy --workspace` could
not be evaluated: a pre-existing `tidepool-codegen` `len_without_is_empty`
clippy failure (also confirmed via `git stash` on the clean tip, a
clippy-version drift unrelated to this change) blocks the whole workspace
build under `-D warnings`, and `tidepool-codegen` is out of this lane's
boundary to fix.

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

**Determinism gap CLOSED (varid-stable lane, 2026-08-22) — ON by default.**
Turning the mechanism on originally exposed a real bug: a cold compile and a
warm (build-products dir) compile of BYTE-IDENTICAL source produced
DIFFERENT output. A structural CBOR diff (node-for-node, not a byte diff)
showed tree SHAPE and node COUNT identical between the two runs; only
`VarId` values differed, at two distinct sites:

1. **Nested (non-top-level) Ids** — `Tidepool.Translate.localVarId` (lambda
   parameters, case binders, local lets) hashed GHC's raw session `Unique`
   directly, and that Unique's value depends on how many uniques earlier
   work in the SAME session already consumed — which a warm `load'` (skip a
   variable number of modules) perturbs. Fixed by
   `Tidepool.Translate.stabilizeLocalUniques`: a pure Core-to-Core pass,
   run once per closed graph, that renumbers every nested Id from a plain
   monotonic counter (no GHC session interaction at all) before
   `localVarId` ever hashes it — see that function's own doc comment for
   the full mechanism and why it needs no change to `localVarId` itself.
2. **Internalized top-level floats** — `GhcPipeline.hs::externalizeInternalTops`
   (the #313 fix) disambiguates by baking the binder's raw Unique into its
   externalized OccName (`_u<unique>`), which is the SAME session-Unique
   dependency reached through `stableVarId`'s hash instead of `localVarId`'s.
   Fixed by switching that suffix to the binder's ordinal position in the
   module's own `mg_binds` (`_t<ordinal>`) — a pure function of source +
   simplifier passes, stable across cold/warm compiles.

Both together make `build_products_dir_cold_warm_identical_output`
(previously `#[ignore]`d) green — byte-identical Core + `DataConTable`,
cold vs. warm. `TIDEPOOL_VARID_AUDIT=1` over both `haskell/test/Suite.hs`
and `haskell/test/corpus/Corpus.hs` (18252 and 6591 binding sites) reports
zero collisions.

**Wired on by default everywhere a `tidepool-extract` gets spawned in
`tidepool-runtime`** via `crate::paths::apply_build_products_dir` — not just
`artifacts.rs::compile_invocation`, but also `session/turn.rs`'s
`run_turn`/`classify_block`/`compile_session_turn` and `session/mod.rs`'s
`validate_candidate` (each builds its own `ExtractCmd`, bypassing
`compile_invocation`'s memo on purpose — a session turn has on-disk side
effects and mutable-session dependencies a content-addressed cache would
get wrong; the build-products dir's module-granular recompilation avoidance
is an orthogonal, additive concern that applies regardless).
`$TIDEPOOL_BUILD_PRODUCTS_DIR` still overrides the location; it is no
longer also the enable switch.

**Observed bench-turn.sh behavior:** a direct manual measurement (5
distinct sources each importing `Tidepool.Prelude`'s full closure, one
shared warm build-products dir vs. none at all) shows the expected win —
~1875ms → ~1697ms average wall per compile (~9.5%) on this box. The
standing `scripts/bench-turn.sh` `session`/`harness` rows, by contrast, show
no measurable turn-over-turn drop in `extract.ghc_load_ms` even with the
mechanism now wired into their spawn path — those scenarios' generated
templates apparently pull in a small enough stdlib closure that GHC's own
fixed session/package-db overhead dominates. Not investigated further here
(out of this lane's scope: the acceptance bar is byte-identical
cold-vs-warm output, not a specific speedup magnitude in every scenario);
worth a follow-up look at what those templates actually import if the
production win doesn't materialize either.

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
