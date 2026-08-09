# Extract timing contract

Measurement only. Nothing here proposes a fix. Supersedes
`11-turn-latency-contract.md` (RETIRED, `git rm`'d in the same change that
landed this doc — see `plans/post-restart/extract-wave/spawn-latency/01-c1-measurement.md`
for the C1 item that did the retiring and the full numbers this doc
summarizes).

## Two lanes, two emitters, never folded together

- **COMPILE lane** — `tidepool-extract --turn` / the whole-module CLI
  (`GhcPipeline.hs`'s `runNormalPipeline`/`runSessionPipeline`). Forwarded
  Rust-side under `extract.<phase>` (`tidepool-harness/src/timing.rs`).
- **CLASSIFY lane** — `tidepool-extract --classify` (`Binders.hs`'s
  `classifyBlock`), a separate, much smaller per-BLOCK spawn. Forwarded
  under `classify.<phase>`.

Never fold `extract.<phase>` into the like-named `classify.<phase>` — two
different subprocess spawns, even where a phase name happens to coincide
(only `ghc_session` does today, see below).

## Phase vocabulary (compile lane, post-partition)

| phase | brackets |
|---|---|
| `startup` | process start → GHC session about to be created |
| `ghc_setup` | session `DynFlags` setup + `guessTarget`/`setTargets` + `depanal` |
| `ghc_load` | GHC's `load' LoadAllTargets` call ALONE — a full first compile of every home module (parse + typecheck + desugar + core2core), no internal decomposition |
| `inject` | SESSION PATH ONLY: PHASE 2's `injectSessionScope`, splicing the live `Val.G<g>` ifaces into the HPT |
| `typecheck` | the SECOND loop's parse+typecheck, summed across every module |
| `core` | the SECOND loop's desugar+core2core, summed across every module |
| `translate` | GHC Core → our `CoreExpr`/`DataConTable` |
| `cbor_encode` | serializing the tree + metadata |
| `write` | writing `<target>.cbor`/`meta.cbor`/`asks.json` |
| `total` | whole-process wall clock |

All FLAT — a collector sums by name, no phase is nested inside another.
`ghc_setup` and `ghc_load` PARTITION what an older `ghc_session` bracket used
to cover on this lane (session setup + `depanal` + `load'`); a historical
compile-lane `ghc_session` figure equals `ghc_setup + ghc_load`, recovered by
the addition any flat-sum collector already performs.

`load'` gets NO internal decomposition. Its entire cost is what C1 measures:
the SECOND loop (`typecheck` + `core`) independently redoes both halves of
what `load'` already did — parse+typecheck+desugar+core2core, over the SAME
home-module set. One row around the whole `load'` call answers "how much of
a turn's extract is the double compile" completely; splitting it further
doesn't change that answer, and GHC's driver doesn't hand you that internal
seam cheaply.

## `ghc_session` — retired on the compile lane, still real on the classify lane

`ghc_session` now denotes exactly ONE span: the `--classify` lane's
`getSessionDynFlags` alone (`Binders.hs`'s `classifyBlock`). The compile
lane's former, much larger use (session setup + `depanal` + `load'`) is
succeeded by `ghc_setup` + `ghc_load`. One emitter, one meaning — the
`extract.`/`classify.` prefix disambiguation is no longer load-bearing for
this specific name (though it still matters for every other phase name the
two lanes share, e.g. `startup`/`typecheck`). Same retirement discipline as
the `classify_extract` tombstone: a same-named phase never silently changes
meaning; a phase that needs a different meaning gets a NEW name instead.

## How to reproduce

Build `tidepool-extract-bin`, then either:

- invoke it directly (`<file.hs> --target <name> --output-dir <dir>` for the
  normal path; `--session-root`/`--inject-val`/`--session-bind` for the
  session path) with `TIDEPOOL_TIMING=1` for a raw phase read, or
- drive `tidepool-harness/examples/turn_latency_bench` (debug profile,
  matches production's `target/debug/tidepool-selfharness`) for turn-shaped
  numbers against the real preamble/stdlib home-module set:

```bash
export PATH=<with-packages GHC>/bin:$PATH
export TIDEPOOL_EXTRACT=<path to tidepool-extract-bin>
export TIDEPOOL_TIMING=1
cargo build --example turn_latency_bench -p tidepool-harness
# GHC-heavy — broker-wrap, never exclusive:
/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- \
  ./target/debug/examples/turn_latency_bench
```

Record `cat /proc/loadavg` and `ps -eo comm= | grep -c '^tidepool-extrac'`
immediately before and after every capture. A bracket taken under heavy box
contention measures contention as much as it measures GHC — see the C1
report's two-load-arm section for why the RATIO survives this and the
absolute milliseconds do not.

## Current numbers, and their coverage

Taken 2026-08-08/09, debug profile, `N=1/size_n=1/retry_n=1`, via the recipe
above. Full detail, raw `tidepool-timing` lines, and the two-load-arm
robustness demonstration are in
`plans/post-restart/extract-wave/spawn-latency/01-c1-measurement.md`; this is
the summary:

- **`ghc_load` (the double compile's first half) is 28–32% of `extract.total`**,
  stable within ~2 percentage points across a box-load swing from a 1-minute
  loadavg of ~10 to ~34 (two arms, 6 samples each). **`ghc_load` is 97–98% of
  `ghc_setup + ghc_load`** in both arms — i.e. almost none of the historical
  "session boot" figure was session boot; it was `load'` running a full first
  compile. `depanal`/session setup proper (`ghc_setup`) is under 3% of
  `extract.total`.
- **The second loop (`typecheck` + `core`) is 62–69% of `extract.total`**,
  stable across both load arms — this REFINES, not replaces, the
  pre-existing 60–66% `core`-alone figure once `typecheck` is folded in as
  part of the same second pass.
- **`ghc_load + second-loop is ~96–97% of `extract.total`** in both arms —
  almost the entire wall clock is two back-to-back full compiles of the same
  home-module set.
- **Coverage caveat, carried forward explicitly:** the pre-existing 60/26/6
  figures this doc's predecessor reported describe a TURN-1-SHAPED extract
  (no injected session Vals — the normal path only; the session path emitted
  NO phases at all before this item). They are NOT directly comparable to
  the numbers above, which were taken after the flat-partition landed and
  cover both paths. Do not average the two vintages together.
- **The `typecheck` row measures only ONE of the TWO typechecks a turn
  actually pays.** `load'` (`ghc_load`) already parses+typechecks+desugars+
  core2cores the full home-module set once; the second loop
  (`typecheck`+`core`) redoes all four steps a second time, independently.
  `ghc_load` gets no internal decomposition (see above), so the FIRST
  typecheck's cost is folded into the single `ghc_load` row, not broken out.
  Treat "how much of a turn is typechecking" as OPEN, not settled by the
  under-6% `typecheck` row alone.
- **Session path** (`runSessionPipeline`, reached via a real `Val`-injecting
  turn): `inject` costs ~0ms at every sampled point in a 5-turn sequence.
  `ghc_setup`/`ghc_load`/`typecheck`/`core` show no visible growth trend
  across that same sequence (noise-dominated at n=5). **Caveat, found by a
  post-hoc self-audit and load-bearing:** no in-process instrument counts how
  many `Val.G<g>` modules a given compile actually injects — the "depth"
  label an earlier draft of the C1 report attached to this data was
  reconstructed from `Wrote session iface: …` output lines in PRIOR spawns,
  which turn out to track only the count of prior BARE-EXPRESSION evals (the
  auto-bound `it` alias), not a verified live-Val count, and are certainly
  not `Lib.G<n>` (session-DECL generation) — the vehicle used never grew past
  one decl module. The decl-generation-depth question (the one E2's O(n²)
  home-module-chain concern is actually about) is UNANSWERED here, and the
  session-VALUE axis is answered less precisely than first reported. See
  `01-c1-measurement.md`'s "Session path" section for the full correction.
