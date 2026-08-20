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
  dominant `--turn` class**, and is now known to be a real lift, not a
  flag-flip: extract runs `backend=noBackend`/`NoLink` and writes NO
  `.hi`/`.o` today. Prerequisites: (1) enable iface writing with a stable
  content-addressed hidir/odir; (2) verify `checkOldIface` recompilation
  checking works under `noBackend` (untested, the real risk); (3) teach
  the manual second pass to reuse `load''`s results, else a cache touches
  only the ~60% upsweep share. Needs its own plan before a lane.

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
  avoidance across spawns — pending `compile-attribution`'s measurement) is
  ALSO the daemon's disk-backed seed; growth is bounded by retirement/rotation
  at quiescent points, mirroring machine rotation.
