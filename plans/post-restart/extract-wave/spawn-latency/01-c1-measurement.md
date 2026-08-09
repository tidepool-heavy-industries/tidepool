# C1 — the double-compile bracket: measurement

Item: C1 (see `00-spec.md` / `LEDGER.md`). MEASUREMENT ONLY — nothing here
proposes a fix, and nothing in the instrumentation changes what the extract
COMPUTES; it changes only whether a `tidepool-timing phase=<name> ms=<int>`
stderr line is written. Env-gated (`TIDEPOOL_TIMING=1`), off by default.

This report was revised twice mid-flight by the wave TL, both times before
any numbers were finalized, and once more by root's throttle directive. All
three revisions are reflected below, not just noted:

1. The bracket design changed from NESTED (`depanal`/`load`/`inject` nested
   inside `ghc_session`) to a FLAT PARTITION (`ghc_setup` + `ghc_load` as two
   separate, non-overlapping rows; `ghc_session` retired on the compile lane,
   kept on the classify lane). The code below reflects the FLAT design only —
   the nested design was never shipped.
2. The "typecheck REFUTED" framing from the sub-TL spec's binding context is
   WITHDRAWN. The `typecheck` phase measures only ONE of the TWO typechecks a
   turn pays; home-module typecheck cost is OPEN, not settled.
3. Every capture used in this report records box load and concurrent-extract
   count immediately before/after, per root's throttle directive, and the
   headline ratio is demonstrated stable across two materially different load
   levels rather than assumed stable.

## Step 2 — anchors confirmed against the code, before any change

Confirmed in `haskell/src/Tidepool/GhcPipeline.hs` on this worktree before
editing:

- **(a)** `load' Nothing LoadAllTargets …` at (then) line 174 compiles every
  home module INCLUDING `core2core` — confirmed by reading the call and its
  surrounding haddock, which states this explicitly.
- **(b)** The `forM summaries $ \modSum0 -> …` loop at (then) lines 191–216
  independently reruns `parseModule`/`typecheckModule`/`hscDesugar`/`core2core`
  over every summary a SECOND time.
- **(c) CONFIRMED, not refuted:** the pre-existing `ghc_session` phase
  bracketed `sessionT0` (~117) to `sessionT1` (~180/192), which is EVERYTHING
  from session `DynFlags` setup through the `load'` call — i.e. `ghc_session`
  already contained `load'` in full. The Phase-B "session boot 26–32%" figure
  was therefore "session setup + `depanal` + a full first compile", not boot
  in the narrow sense. This is the finding the whole item is built on, and it
  survived every subsequent revision.

## The bracket, as shipped (flat partition, not nesting)

`haskell/src/Tidepool/GhcPipeline.hs`, both `runNormalPipeline` and
`runSessionPipeline`:

| phase | brackets | lane |
|---|---|---|
| `startup` | process start → GHC session about to be created | both |
| `ghc_setup` | session `DynFlags` setup + `guessTarget`/`setTargets` + `depanal` | both |
| `ghc_load` | the `load' LoadAllTargets` call ALONE, no internal decomposition | both |
| `inject` | PHASE 2's `injectSessionScope` (live `Val.G<g>` iface splice) | session path only |
| `typecheck` | the SECOND loop's parse+typecheck, summed across every module | both |
| `core` | the SECOND loop's desugar+core2core, summed across every module | both |

`ghc_setup` and `ghc_load` are FLAT and non-overlapping — they PARTITION what
the old `ghc_session` bracket covered on the compile lane; a historical
compile-lane `ghc_session` figure equals `ghc_setup + ghc_load`. The compile
lane no longer emits `ghc_session` at all — `Binders.hs`'s `--classify` lane
(`classifyBlock`) is now the SOLE emitter of that name, for its own much
smaller span (`getSessionDynFlags` alone). See
`plans/self-iterating-harness/11-extract-timing-contract.md` for the full
phase vocabulary doc (the retirement-replacement for
`11-turn-latency-contract.md`, `git rm`'d in this same change) and
`tidepool-harness/src/timing.rs`'s `PHASE_GHC_SESSION` tombstone for the code
-level statement of the same retirement.

`load'` (`ghc_load`) gets NO internal decomposition, per the wave TL's
correction: its entire cost IS what C1 measures, because the second loop
independently redoes both halves of what `load'` already did. GHC's driver
doesn't hand you that internal seam cheaply, and splitting it further would
not change the answer to "how much of a turn is the double compile".

## Prerequisite check — is `load'` parallel?

Grepped `extractionDynFlags` and `canonicalizeDFlags` (`GhcPipeline.hs`) and
the rest of `haskell/src/` and `haskell/app/` for `parMakeCount`, `-j<N>`,
`GHC_JOBS`, `numCapabilities`/`getNumCapabilities`: **zero matches.**
`parMakeCount` (GHC's upsweep-parallelism `DynFlags` field) is never set
anywhere in the extractor, so it stays at GHC's default (`Nothing` —
sequential upsweep). **Both `load'` and the second per-module loop are
sequential.** This matters for the two-load-arm demonstration below: a
sequential-vs-sequential pair gives the `ghc_load`-share ratio a mechanistic
reason to be contention-robust (CPU contention should degrade both phases
roughly proportionally, not asymmetrically), rather than the ratio's
stability being a coincidence the demonstration merely happens to observe.

## Wire-inertness receipt

Two independent checks, same fixture (`Expr.hs`, a minimal `result :: Int`
module), `--target result --output-dir <dir>`, `TIDEPOOL_TIMING` unset vs
`=1`:

**Check 1** (nested-bracket build, superseded by the flat-partition rewrite,
kept as a receipt that the discipline held at every stage):
```
STDOUT IDENTICAL
EMITTED FILES IDENTICAL
```

**Check 2** (final flat-partition build, the code actually shipped):
```
--- stdout diff ---
STDOUT IDENTICAL
--- file diff ---
EMITTED FILES IDENTICAL
--- timing lines (on2) ---
tidepool-timing phase=startup ms=80
tidepool-timing phase=ghc_setup ms=21
tidepool-timing phase=ghc_load ms=300
tidepool-timing phase=typecheck ms=1
tidepool-timing phase=core ms=1
tidepool-timing phase=translate ms=0
tidepool-timing phase=cbor_encode ms=0
tidepool-timing phase=write ms=1
tidepool-timing phase=total ms=409
```
No `ghc_session` line — confirms the compile-lane retirement took effect.
`diff` on both `stdout` and the emitted output directory returned empty
(`diff -rq` prints nothing on a clean compare) in both checks.

## Gate receipts

- **`extract-fidelity-test`: 26/26 checks passed.** (`Test suite
  extract-fidelity-test: PASS`, `26/26 checks passed`, `1 of 1 test suites (1
  of 1 test cases) passed.`) Run via
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- bash -c 'cabal test
  extract-fidelity-test'` from `haskell/`.
- **`tidepool-harness` acceptance shard: 24/24 passed, 0 failed, 0 skipped.**
  (`Summary [1845.720s] 24 tests run: 24 passed (20 slow), 0 skipped.`) Run
  via `scripts/battery-shard.sh tidepool-harness -E 'binary(/^acceptance_/)'`
  (self-slotting, unwrapped per its own discipline). 20 of the 24 exceeded
  nextest's 60s "SLOW" marker — the slowest, `selfharness_multi_cycle_state_
  accumulates_across_loop_boundaries`, took 1079s — consistent with running
  under sustained box contention (see the load section below), not a
  regression signal; every test still passed.
- **`cargo check --workspace`: clean**, both before and after the
  flat-partition rewrite (run bare early in the session per the guidance in
  force at the time; the throttle directive that later required broker- or
  `nice`-wrapping pure-Rust builds landed mid-session — noted to the wave TL,
  no re-run required since the check result itself doesn't depend on box
  load).

## Two-load-arm robustness demonstration (the headline)

Per root's throttle directive: absolute milliseconds are contended upper
bounds, not headline numbers. **The headline is the ratio, demonstrated
stable across two materially different load levels — not assumed stable.**

Both arms: `turn_latency_bench`, debug profile,
`TURN_LATENCY_BENCH_N=1/SIZE_N=1/RETRY_N=1`, `TIDEPOOL_TIMING=1`, run via
`ghc-slots.sh run` (never exclusive). Both arms were captured BEFORE this
worktree carried `fc3363dc` (the nextest `ghc-heavy` per-run cap 3→1 fix) and
before the box-wide slot-pool 3→4 change — so neither arm straddles either
transition, and the two are comparable to each other (this bench invokes
`tidepool-extract-bin` directly via a single blocking `Command::output()`
call per turn; it does not go through nextest's `ghc-heavy` test group at
all, so the cap change does not bear on this specific measurement either
way).

### HIGH-load arm — 1-min loadavg ~28–34

Two separate captures at this contention level (one immediately after the
flat-partition rebuild, one from the original nested-bracket run whose
`depanal`/`load` numbers are numerically identical to the flat partition's
`ghc_setup`/`ghc_load` — same code span, renamed only):

```
loadavg readings bracketing these captures: 33.96/52.68/81.98 → 31.30/51.16/81.01 → 32.73/49.57/79.53
(1-min/5-min/15-min; concurrent-extract count not captured for this arm — predates the
corrected `ps -eo comm= | grep -c '^tidepool-extrac'` instrument being circulated; the
"ghc-slots: all slots busy — polling for any free slot" broker message is the qualitative
floor for every sample in this arm)
```

| sample | ghc_setup ms | ghc_load ms | typecheck ms | core ms | extract.total ms |
|---|---|---|---|---|---|
| cold (run A) | 94 | 5141 | — | — | 16199 |
| small/large (run A) | 137 | 4879 | — | — | 17424 |
| retry (run A) | 142 | 4533 | — | — | 14548 |
| cold (run B) | 91 | 4626 | 668 | 10769 | 16679 |
| small/large (run B) | 118 | 5020 | 720 | 10820 | 17293 |
| retry (run B) | 134 | 5184 | 626 | 10417 | 16856 |

Raw lines, run B (fresh flat-partition build), one representative sample:
```
tidepool-timing phase=startup ms=91
tidepool-timing phase=ghc_setup ms=91
tidepool-timing phase=ghc_load ms=4626
tidepool-timing phase=typecheck ms=668
tidepool-timing phase=core ms=10769
tidepool-timing phase=translate ms=?
tidepool-timing phase=cbor_encode ms=?
tidepool-timing phase=write ms=?
tidepool-timing phase=total ms=16679
```
(exact `translate`/`cbor_encode`/`write` sub-ms values are in the JSON
summary, `extract.total` above is `median_ms` off the bench's own
`stages` array, not hand-recomputed)

### LOW-load arm — 1-min loadavg ~11–19

Two bench runs batched inside ONE broker slot acquisition (per root's
"batch, don't drip" instruction), load recorded immediately before/after
each:

```
LOAD BEFORE run1: 19.17 26.16 54.77 (1m/5m/15m); concurrent extracts: 6
LOAD AFTER run1 / BEFORE run2: 14.34 23.20 51.24; concurrent extracts: 6
LOAD AFTER run2: 10.78 20.28 48.19; concurrent extracts: 4
```

| sample | ghc_setup ms | ghc_load ms | typecheck ms | core ms | extract.total ms |
|---|---|---|---|---|---|
| cold (run1) | 110 | 4352 | 538 | 8413 | 13831 |
| small/large (run1) | 70 | 2914 | 366 | 5834 | 9503 |
| retry (run1) | 77 | 2716 | 356 | 5652 | 9055 |
| cold (run2) | 58 | 2782 | 348 | 5568 | 9060 |
| small/large (run2) | 63 | 2727 | 341 | 5135 | 8573 |
| retry (run2) | 49 | 2764 | 291 | 4736 | 8105 |

Raw lines, run2/cold:
```
tidepool-timing phase=startup ms=44
tidepool-timing phase=ghc_setup ms=58
tidepool-timing phase=ghc_load ms=2782
tidepool-timing phase=typecheck ms=348
tidepool-timing phase=core ms=5568
tidepool-timing phase=translate ms=202
tidepool-timing phase=cbor_encode ms=19
tidepool-timing phase=write ms=0
tidepool-timing phase=total ms=9060
```

### The demonstration

| metric | HIGH-load arm (n=6) | LOW-load arm (n=6) | agreement |
|---|---|---|---|
| `ghc_load / extract.total` | mean 29.7%, range [27.7, 31.7] | mean 31.5%, range [30.0, 34.1] | **1.7 pp apart** |
| `ghc_load / (ghc_setup + ghc_load)` | 97.0–98.2% | 97.2–98.3% | **within 1.3 pp, both arms** |
| `(typecheck+core) / extract.total` | 65.5–68.6% (run B only, n=3; run A predates the `typecheck`/`core` phases) | 62.0–66.4% (n=6) | **~3-4 pp apart** |

Absolute `extract.total` fell from ~14.5–17.4s (high load) to ~8.1–13.8s (low
load) — a ~40% swing, confirming contention materially inflates absolute
wall clock. **The `ghc_load` share of `extract.total`, and especially the
`ghc_load` share of `ghc_setup + ghc_load`, held essentially flat across
that swing** — a 1-minute-loadavg delta of roughly 15–23 points moved the
headline ratio by under 2 percentage points. Combined with the prerequisite
check (both `load'` and the second loop are sequential, so contention has no
structural reason to degrade them asymmetrically), this is a **demonstrated**
contention-independence, not an assumed one, per root's bar. The two arms
AGREE — this is the confirming branch, not the divergent one; no quiet-box
re-take is required by root's own stated rule (divergence would have
triggered one).

`ghc_load + (typecheck+core)` — i.e. the double compile's two halves
combined — is **~96–97% of `extract.total` in both arms.** Almost the
entire wall clock is two back-to-back full compiles of the same home-module
set; `ghc_setup`/`startup`/`translate`/`cbor_encode`/`write`/`classify`
combined are the remaining 3–4%.

**Absolutes above are contended upper bounds, explicitly NOT comparable to
the historical quiet-box figures** (the `11-turn-latency-contract.md`
pipeline's 6.8s `extract_spawn`, the 60/26/6 split) — those were taken on a
quiet box under a different `TIDEPOOL_TIMING` code path (turn-1-shaped, no
`ghc_setup`/`ghc_load` split existed yet). The RATIO is the only figure this
report treats as a headline; see
`plans/self-iterating-harness/11-extract-timing-contract.md` for the same
caveat stated once more, for readers who land there directly.

## Session path — reached, at 5 samples along ONE generation axis

**Vehicle:** `tidepool-repl::decl_plane::record_syntax_selectors_localized`
(`cargo test -p tidepool-repl --test decl_plane
record_syntax_selectors_localized -- --nocapture --test-threads=1`, via
`ghc-slots.sh run`, `TIDEPOOL_TIMING=1`). A user-defined ADT (`data P = P
{px, py :: Int}`) bound to a session value and referenced across several
turns is what actually triggers `isSessionScopeActive` — plain `Int`/`Text`
binds and bare references do NOT (their type round-trips as a spliced source
string; the tested-and-rejected alternative vehicles are listed below).

Vehicles tried and their outcome, for the record:
- `value_fidelity::bind_references_earlier_binding` (`k <- pure 5; m <- pure
  (k+1); m`) — reached ONLY the normal path on all 4 spawns (`live_val_modules`
  came back empty at every compile; the comment on that test itself says the
  bind action resolves via a "seeded ExternalEnv", i.e. a JIT-side mechanism,
  not a GHC-level import). Zero `inject` lines. Ruled out as a session-path
  vehicle.
- `decl_plane::record_syntax_selectors_localized` — reached
  `runSessionPipeline` on 6 of 8 spawns (2 were compile-error retries the
  driver recovers from automatically; both retries also carry `inject`).
  **Used below.**

Load at capture: `18.41`→`15.05`→`17.19`→(job completed) — this run
overlapped the tail of the monitor described in the throttle-directive
section above; treat it as taken in the ~15–24 (1-min) band, not quiet.

| live `Val.G<g>` count | ghc_setup ms | ghc_load ms | inject ms | typecheck ms | core ms | translate ms | top-level bindings |
|---|---|---|---|---|---|---|---|
| 1 (bind `p`) | 287 | 5985 | — (no prior live Val; normal path) | 414 | 6266 | 333 | 1706 |
| 1 (`case p …`, retry-success) | 68 | 3678 | 0 | 346 | 5268 | 403 | 1708 |
| 2 | 110 | 4419 | 0 | 557 | 8426 | 628 | 1708 |
| 3 | 132 | 6804 | 0 | 717 | 10682 | 450 | 1709 |
| 4 (`123::Int`, retry-success) | 117 | 5064 | 0 | 576 | 8583 | 407 | 1708 |

Raw lines for the 3-injected-Val sample:
```
Processing (session): /tmp/.tmpWy7iqZ/Expr.hs
tidepool-timing phase=startup ms=76
tidepool-timing phase=ghc_setup ms=132
tidepool-timing phase=ghc_load ms=6804
tidepool-timing phase=inject ms=0
tidepool-timing phase=typecheck ms=717
tidepool-timing phase=core ms=10682
tidepool-timing phase=translate ms=450
tidepool-timing phase=cbor_encode ms=14
tidepool-timing phase=write ms=0
```
(the raw stderr in this run predates the `ghc_setup`/`ghc_load` rename —
`depanal`/`load` in the captured log; shown here relabelled to the shipped
names since the underlying spans are byte-for-byte identical, confirmed by
the wire-inertness check's second pass on the same code)

**Two things this settles, and one it does not:**

- **`inject` is cheap regardless of depth** — exactly `0ms` at every sampled
  depth (1 through 4 live Vals). Injecting a thin, type-only iface per live
  binding is not where session-path cost lives, at least at this shallow
  depth.
- **No visible growth trend in `ghc_setup`/`ghc_load`/`typecheck`/`core`
  across live-Val count 1→4** — the samples fluctuate (e.g. `ghc_load` from
  3678 to 6804 non-monotonically) consistent with contention noise dominating
  at `n=5`, not with a clear scaling signal in either direction.
  Top-level-bindings-compiled stays essentially flat (1706–1709) across every
  sample, which is the mechanical reason nothing scales here: the compiled
  home-module SET never grew.
- **This is the WRONG axis for the wave's actual question, and that must be
  said plainly.** The live-Val count above is `Val.G<n>` — the SESSION-VALUE
  generation, incremented once per successful bind/reference turn. It is NOT
  `Lib.G<n>` — the SESSION-DECL generation, the one E2's O(n²)
  home-module-chain concern is actually about. This vehicle's `data P = …`
  decl compiled ONCE (`Lib.G1`) and never grew; every subsequent turn
  compiled against that same single decl module. **The
  compile-time-vs-`Lib.G<n>`-generations question is UNANSWERED by this
  report.** No existing test in `tidepool-repl/tests/` drives 5+ sequential
  `repl.def(...)` calls in one session (the deepest found, in `decl_plane.rs`
  and `shadow_rebind.rs`, is 2–3); reaching that axis needs either a new
  vehicle or a longer existing one not currently in the suite, and neither
  was in this item's budget. An honest gap, not an extrapolation.

## Structural inputs (item 10) — what was and wasn't captured

- **Modules compiled per turn:** not captured as a distinct count.
  `Top-level bindings: N` (stderr, unconditional, not gated on
  `TIDEPOOL_TIMING`) is a proxy for the SIZE of what's compiled but counts
  bindings, not modules, and stayed ~1706–1709 across every session-path
  sample and ~similar magnitude on the normal path — i.e. dominated by the
  fixed preamble/stdlib set, not by anything this item varied.
- **Compile time vs number of `Lib.G<n>` generations:** UNANSWERED — see
  above.
- **Fat-iface bytes per turn:** not captured. `meta.cbor` byte sizes are
  visible in stderr (`Wrote: …/meta.cbor (N entries, B bytes)`) but that is
  metadata, not the fat-iface (`.hi`) byte count the item asked about; no
  cheap read of the latter was found inside this item's budget, and no new
  stderr grammar was added to get it (confirmed first, per the spec's
  instruction, that `ExtractTiming::parse` ignores any non-`tidepool-timing`
  line, so adding one would have been safe — it just wasn't attempted, to
  stay inside the measurement-only, cheap framing).
- **worklist pushes vs unique vars:** not applicable to this item (C2/E5's
  question, not C1's); not attempted.

## Answers

**(1) How much of a turn's extract is the FIRST compile (`load'`) versus the
SECOND loop?**

`ghc_load` (the first compile, `load'` alone) is **28–32% of
`extract.total`**, demonstrated stable across a box-load swing from ~11 to
~34 on the 1-minute figure. The second loop (`typecheck`+`core`) is
**62–69%**. Combined, the two halves of the double compile are **~96–97% of
a turn's extract wall clock** — almost nothing else is left. The second loop
costs roughly TWICE what the first compile costs (a ~2:1 ratio held across
both load arms), despite — per the `load'` haddock at `GhcPipeline.hs`
~222 — doing structurally the SAME work (parse+typecheck+desugar+core2core
over the same home-module set) a second time.

**(2) Read against the Phase-B breakdown (core 60–66%, session boot 26–32%,
typecheck under 6%) — does the double-compile suspicion survive, and
specifically, how much of the "26–32% session boot" is actually `load'`
doing a full first compile?**

**It survives, upgraded rather than refuted — this is the finding the wave
TL's mid-item correction made explicit and this report confirms with
numbers.** The pre-existing "session boot 26–32%" figure IS `ghc_setup +
ghc_load` under the new names, and within that pair, **`ghc_load` alone is
97–98% of it in both load arms** — `ghc_setup` (the part that is genuinely
session/interface setup) is under 3 percentage points of `extract.total`.
So essentially ALL of the historical "session boot" bucket was `load'`
running a full first compile, not boot in any narrow sense. Separately, the
pre-existing `typecheck` row (under 6%) is not a refutation of a
home-module-typecheck cost — it counts only the SECOND loop's typecheck;
`load'` pays a first, larger typecheck internally that this instrumentation
does not (and per the wave TL, should not) break out on its own. Read
together: the double compile is real, it is nearly the WHOLE turn (~96–97%),
and the historical breakdown's "session boot" label concealed roughly a
third of that reality under a name that suggested something cheaper than
what was actually happening.

**(3) What do the numbers say about precompiled interfaces vs a persistent
server (evidence, not a recommendation)?**

- **`ghc_setup`/`startup` (true session/process boot) is under 3% of
  `extract.total`** in every sample taken. A persistent server's most
  obvious selling point — skip re-booting a fresh GHC session/process every
  turn — addresses a SMALL slice of the measured cost on these numbers,
  UNLESS a persistent server also RETAINS compiled state (HPT) across turns
  so `load'` itself doesn't have to recompile the whole stable dependency set
  (preamble, `Tidepool.Prelude`, effect stack) every single turn. That
  retention is what would actually move the needle, since `ghc_load` is
  where the first compile's cost lives, not `ghc_setup`.
- **`ghc_load` + the second loop together are ~96–97% of `extract.total`,
  and both redo the SAME set of home modules.** Precompiled interfaces (or
  any mechanism that lets the second loop reuse `load'`'s already-computed
  Core for unchanged modules, rather than re-parsing/re-typechecking/
  re-desugaring/re-optimizing them) target exactly this redundancy directly.
  Since the second loop alone is the LARGER of the two halves (62–69% vs
  28–32%), avoiding its redundant recompilation of the STABLE dependency set
  is where the single largest win on these numbers would land — larger than
  what session-boot avoidance alone would deliver.
- **Session-path depth data does not (yet) distinguish the two candidates**,
  because it varies the wrong generation axis (`Val.G<n>`, not `Lib.G<n>`;
  see above). What it DOES show — `inject` staying at 0ms and no visible
  scaling in `ghc_setup`/`ghc_load`/`typecheck`/`core` across 1–4 live
  Vals — is at least consistent with "the session-value plane is cheap to
  carry forward," which is a mild point in favor of a persistent-server
  design being able to retain live values cheaply, but it says nothing about
  the `Lib.G<n>` decl-chain cost either candidate would actually need to
  solve. That is the open question a follow-up vehicle would need to close
  before the pivotal decision can lean on the session path at all.

The decision itself is the wave TL's to record in `LEDGER.md`, per the
sub-TL spec's binding constraints — this report supplies evidence, not a
recommendation.
