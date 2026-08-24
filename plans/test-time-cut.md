# GHC-heavy test wall-time cut plan

Status: DIAGNOSIS COMPLETE, awaiting operator pick. No tests changed by this
lane — see "Method" for the read-only instrumentation used.

Operator ask (2026-08-24): cut ~90% of GHC-heavy test wall time, primarily by
batch-compiling multiple tests' `.hs` into single compile actions. This
document is the measured answer to "where does the time go", plus a ranked
plan the operator picks from before anything is restructured.

## Bottom line

**Compiling more targets in one spawn is nearly free; the spawn itself is
not.** A 3-target `--targets a,b,c` spawn costs the same wall time as a
1-target spawn (measured: 4.85s / 4.96s / 5.13s for 1/2/2-target spawns of
the same module — §3). GHC session startup + typecheck dominates; codegen for
an extra target is noise. That is the mechanical basis for the ~90% target
being *reachable on the batchable slice* — but roughly 40% of sampled compile
wall time is **session-scoped** (`--session-root`/`--inject-val`), which the
compile memo already excludes by design (`tidepool-runtime/CLAUDE.md`'s
"Compile cache" section — session-scope flags are not on the argv allowlist,
so such an invocation keys to `None` and always compiles cold)
and which target-batching does not touch either: those compiles read live,
per-turn mutable session state, so there is nothing fixed to batch two of
them against. **A 90% cut is achievable on the eval/fixed-template slice, not
on the whole suite** — see §6 for the honest total.

## Method (reproducible)

A shell shim (`extract-shim.sh`, kept under this worktree's scratchpad, never
committed — see "What changed" at the end) wraps the real `tidepool-extract`
binary: it logs one JSONL line per invocation (wall time, exit code,
`sha256` of the positional input file's CONTENT, whether `--session-root`/
`--inject-val` is present) then execs the real binary unchanged. Set
`TIDEPOOL_EXTRACT=<shim path>` and `REAL_TIDEPOOL_EXTRACT=<real extract>`;
every spawn any caller makes is transparently logged with zero code changes.
This observes the SAME thing `tidepool_extract_cmd::extract_spawn_count`
counts (the one spawn site), from outside the process, so it works across
nextest's one-process-per-test model without touching any crate.

Each sampled binary ran via `scripts/battery-shard.sh <crate> -E
'binary(<name)'`, sequentially (one at a time, respecting the box's
`ghc-slots` semaphore), with `TIDEPOOL_COMPILE_CACHE_DIR` pointed at a
scratch dir and `XDG_CACHE_HOME` isolated per run. Cap was raised to 4 before
sampling (cherry-picked `87611006` onto this branch per operator instruction,
so these numbers reflect the current committed cap, not the stale cap=2).

Raw logs: `<scratchpad>/measure/logs/*.jsonl` (one line per real
`tidepool-extract` spawn: `elapsed_ms`, `content_hash`, `session_scoped`,
full `argv`). Anyone can reproduce by re-running the same
`extract-shim.sh` wrapper — it's a ~40-line bash script, reconstructable from
this doc's description if the scratch copy is gone.

## 1. Spawn census (sampled binaries)

| Binary (crate) | nextest tests | real spawns | extract wall | session-scoped | distinct content | wall clock (nextest) |
|---|---|---|---|---|---|---|
| `acceptance_boot_compile_count` (harness) | 1 | 2 | 17.1s | 2/2 | 2/2 | 19.5s |
| `acceptance_cross_turn` (harness) | 10 | 49 | 197.6s | 31/49 (63%) | 30/49 | 66.9s (4-way parallel) |
| `acceptance_multi_target` (harness) | 2 | 3 | 14.9s | 0/3 | 1/3 | 10.8s |
| `jit_surface` (runtime) | 42 | 45 | 346.9s | 0/45 | 45/45 | 105.1s (4-way parallel) |
| `e2_stowed_machine` (handlers) | 3 | 3 | 11.9s | 0/3 | 3/3 | 5.2s |
| `batch_turns_spawn_census` (repl) | 1 (measures 6 item-shapes internally) | 31 | 123.5s | 22/31 (71%) | 30/31 | 126.8s (sequential by design) |
| **Sample total** | **59** | **133** | **712.0s** | **55/133 (41%)** | **109/133 (82%)** | — |

Two spawns per binary were near-zero (`--version`/no-arg probes the
toolchain-availability check makes) and are excluded from "real spawns"
above — they cost single-digit milliseconds and never touch GHC.

**Session-scoped vs. non-session compiles cost the same per spawn**
(measured: 5.39s avg session-scoped vs. 5.32s avg non-session, across the
full sample) — session compiles are not slow because of what they do, they
are slow because every one of them is a full GHC invocation, same as any
other. The distinguishing property is *cacheability*, not cost.

**Which binaries are session-heavy vs. eval-heavy is a real crate-level
split, not noise:**
- **Harness/repl turn-lane tests** (`acceptance_cross_turn`, the repl census)
  are 63–71% session-scoped — every turn's compile injects that session's
  live `Val.G<g>` state, so most of their compile cost is structurally
  uncacheable today.
- **Runtime eval-lane tests** (`jit_surface`) are 0% session-scoped and 0%
  duplicated within the binary (45 distinct hashes / 45 real spawns) — every
  probe compiles genuinely different Haskell source on purpose (that's the
  point of an eval surface test), so the compile memo can never help these
  *within one binary run*; the only lever is fewer, denser spawns.
- **Boot/multi-target tests** are the batching success story already landed:
  `acceptance_multi_target`'s 1/2/2-target spawns cost 4.85s/4.96s/5.13s —
  batching two extra targets into one spawn cost ~5% more wall time, not 2x.

## 2. Duplication map

Across the 133 sampled real spawns, content-hash groups to **109 distinct
compile units** (82%) — meaning roughly a fifth of sampled spawns are
re-compiling byte-identical source. That fifth splits sharply by cacheability:

- **`acceptance_cross_turn` alone**: 49 spawns → 30 distinct (39% duplicate),
  and of the 19 "extra" spawns, the large majority are non-session compiles
  (11 distinct among 18 non-session spawns = 7 duplicate spawns) *plus*
  duplicate content among the 31 session-scoped spawns (24 distinct among 31
  = 7 more duplicate spawns) — i.e. even the uncacheable session lane
  recompiles identical-looking WRAPPER SOURCE repeatedly across turns within
  one test binary, it just can't be served from the memo because the
  embedded session state differs by construction (see §5).
- **`jit_surface`**: 0% duplication — every eval probe is deliberately unique
  content. The memo has nothing to hit here; only the spawn *count* is a
  lever (§4).
- **Cross-binary** (all 6 sampled binaries sharing one scratch memo dir):
  109 distinct / 133 total is dominated by within-binary uniqueness
  (jit_surface's 45 unique probes are ~34% of the whole sample on their own),
  so this sample under-counts the REAL cross-process win the harness's own
  `tidepool-harness/CLAUDE.md` already measured directly: **119s → 99s cold
  → 47s warm** on `golden_path + acceptance_askuser + selfharness_spine`
  (fixed boot/turn-template compiles shared across processes). That number is
  the actual state of the art for what the memo already delivers when
  triggered by identical FIXED-source compiles across many test processes —
  our sample didn't reproduce it because none of the 6 binaries we sampled
  share a boot/turn-wrapper compile with each other by design (deliberately
  chosen to cover breadth, not to re-demonstrate memo sharing already
  documented elsewhere).

**Bottom line on duplication:** the compile memo (already landed — see
`tidepool-runtime/CLAUDE.md`'s "Compile cache" section) is doing its job on
the FIXED-template slice
(boot/turn wrappers) — that's a solved problem with a measured number (47s
warm vs 119s before). The remaining duplication inside `acceptance_cross_turn`
is smaller and mixed between "genuinely cacheable, just not yet warm in this
run" and "structurally uncacheable" (session-scoped). There is no large
undiscovered duplication pool waiting on a keying fix.

## 3. Batch-compile opportunity

`compile_targets(source, targets: &[&str], include, bin, on_stage)`
(`tidepool-runtime/src/artifacts.rs:470`) already supports N targets against
ONE shared module in ONE spawn — it's the production front door for the
harness turn lane and is exactly the `--targets a,b,c` extract mode. Measured
marginal cost of an extra target in the SAME spawn:

| targets in one spawn | wall |
|---|---|
| 1 | 4.85s |
| 2 | 4.96s |
| 2 (one bad name, fails) | 5.13s |

Adding a second target cost **+110ms (2%)**. This is the mechanical
justification for "batch multiple tests' `.hs` into one compile unit": GHC
session startup + typecheck of the shared module dominates; each additional
top-level binding compiled alongside it is nearly free.

**What this means for consolidation:** any family of `#[test]` fns that
currently each call `compile_and_run`/`compile_haskell` on their OWN small
module could instead share ONE module (many top-level bindings, one per
assertion) compiled with `compile_targets` in ONE spawn, each assertion
reading its own named target's CBOR out of the shared `meta.cbor`. This is
the SAME shape `jit_surface.rs`'s family tests
(`works_prelude_core_family`, etc.) already use at the SOURCE level — a
single `src` string covering many checks compiled through `compile_and_run`
once — but `compile_targets`'s N-target mode is a further step: instead of
folding N checks into ONE expression result, each check gets ITS OWN target
name and its own typed CBOR, which is more legible when checks need
DIFFERENT expected types (the family-table pattern requires flattening every
check to the same result shape; multi-target does not).

**Ceiling:** a fixture-build step is bounded by how many DISTINCT modules
can share one GHC session before name collisions or unrelated compile
failures start coupling unrelated tests together (root CLAUDE.md already
flags this cost: "a bundled crash destroys sibling diagnosis" — multi-target
inherits the SAME all-or-nothing contract `compile_invocation`'s doc states
explicitly: "a nonzero exit fails the WHOLE spawn if ANY target can't
translate"). Observed real ceiling: extract already compiles 41 stdlib
modules in ~3.1s during ordinary startup (repl census log) and 33 modules in
7.8s was previously observed (root CLAUDE.md) — so GHC's own module-count
scaling is not the binding constraint; test-isolation blast radius is.

## 4. Shape census

| Crate | GHC-heavy binaries | GHC-heavy `#[test]` fns | Standalone-flagged (grep signal)* |
|---|---|---|---|
| tidepool-harness | 41 | 162 | 3 `#[ignore]`, 2 compile_fail-named |
| tidepool-runtime | 61 | 714 | 6 `#[ignore]`, 2 proptest!, 5 compile_fail-named, 1 sigill-named |
| tidepool-repl | 27 | 141 | 0 |
| tidepool-handlers | 5 | 27 | 0 |
| tidepool-testing | 2 | 13 | 1 `#[ignore]` |
| **Total** | **136** | **1057** | **~20 files** |

\* Grep signal only (`#[ignore]`, `proptest!`, `compile_fail`/`trybuild`,
crash/panic/sigill-named files) — root CLAUDE.md's full standalone-stays-
correct list also includes "distinct fixtures", which needs per-binary
reading, not a grep. This census is NOT an exhaustive per-binary
classification of all 136 binaries (out of budget for this lane) — it
establishes the SCALE (only ~15% of files show an obvious standalone signal;
the rest are candidates for review) and the PATTERN, demonstrated on 6
representative binaries:

- **Already well-consolidated (reference model):** `jit_surface.rs`'s module
  doc states its own history plainly — "One compile per trivial pure probe
  used to be the norm here (~97 `#[test]`s)"; it now runs 42 `#[test]` fns
  (down from 97) via family-bundle tests (`works_prelude_core_family`, etc.)
  that fold many small checks into ONE compiled module per family, alongside
  ~30 still-standalone tests that check DISTINCT behaviors (specific error
  messages, specific edge cases) — correctly kept separate because they are
  not "one more value check", they are testing different mechanisms.
  Measured spawn count (45) tracks test count (42) almost 1:1 even AFTER
  consolidation — meaning the family-bundle pattern already reduced the
  PROCESS count (97→42, the win nextest actually monetizes, see §5) but each
  remaining `#[test]` still pays close to one spawn. Extending family tests
  to `compile_targets`'s N-in-one-spawn mode (§3) is the next increment here,
  not a new pattern.
- **`acceptance_multi_target`, `batch_turns_spawn_census`, boot-compile-count:
  purpose-built measurement tests**, not consolidation targets — they exist
  specifically to pin the spawn count of a real path and would defeat their
  own purpose if merged into a shared fixture.
- **`e2_stowed_machine`**: 3 tests, 3 distinct one-off compiles (GC-hazard
  probe, suspend/resume, abort) — plausible bundle candidate (share one
  compiled turn across the 3 assertions if the 3 assertions can act on the
  SAME compiled program with different runtime choreography around it), but
  this is a judgment call on the test's actual intent, not read here in
  depth.
- **`acceptance_cross_turn`**: 10 tests, 49 spawns, structurally
  MULTI-TURN by design (each test drives several sequential turns to prove a
  cross-turn property) — NOT a family-bundle candidate in the jit_surface
  sense, because the whole point is sequencing distinct turns, not checking
  independent facts. Consolidation lever here is different: see §5.

## 5. Process model (nextest audit — operator addendum)

**(a) Process-per-test is load-bearing, not bloat.** Confirmed by reading
`.config/nextest.toml`'s own header: it structurally de-races the JIT's
process-global state (signal handlers, GC, fork-safety harnesses, and the
`EXTRACT_SPAWNS` counter itself, which several purpose-built tests — boot
compile count, the repl census — rely on being PROCESS-GLOBAL and
uncontaminated by sibling tests). None of this measurement found a reason to
question it. Do not propose removing it.

**(b) `ghc-heavy` cap A/B: DROPPED from scope per operator ruling.** Cap is
already raised 2→4 (`87611006`, committed; cherry-picked onto this branch so
this report's own sampling ran at cap=4). No further A/B needed. Peak memory
was not specifically instrumented (out of scope per the ruling), but no
sampled run showed memory pressure — `free -h` mid-sampling showed 21Gi free
of 31Gi total on a box also running two other concurrent dev lanes'
GHC-heavy tests.

**(c) Setup scripts as the batch-compile integration point — feasibility
verified, recommend a SCOPED PILOT, not a blanket adoption.**
- **Maturity:** nextest's setup-scripts feature is **EXPERIMENTAL** in the
  pinned version (`cargo-nextest 0.9.138`) — it requires `experimental =
  ["setup-scripts"]` in `.config/nextest.toml` (a config change, out of this
  lane's boundary) and is documented as "not yet stable." Config syntax as
  of nextest ≥0.9.98 (current, post-deprecation of the old top-level
  `[script.*]` form): `[scripts.setup.<name>]` with a `command`, activated
  per-profile via `[[profile.default.scripts]]` with a `filter` (rdeps-style
  test selector) and `setup = "<name>"`.
- **Artifact flow through the memo:** a setup script can pass state to tests
  ONLY via environment variables written to `$NEXTEST_ENV` (no other
  channel) — so a setup script that runs ONE `tidepool-extract` invocation
  ahead of a binary's tests would need to either (i) point
  `TIDEPOOL_COMPILE_CACHE_DIR` at a location the setup script pre-populated
  (this flows cleanly: the memo is content-addressed and already
  path-independent by design, so a setup-script-warmed memo dir is
  indistinguishable from a warm-from-a-prior-run one — no new mechanism
  needed), AND (ii) build its warm-up compile from byte-identical
  source+argv to what each test's own `compile_targets`/`compile_haskell`
  call will build, or the keys miss and the setup script bought nothing.
  Condition (ii) is real fixture-engineering work, not a config toggle — the
  setup script's compiled module would need to BE the shared family-bundle
  module the tests already agree on (§3/§4's consolidation work), not an
  independent thing.
- **Recommendation: pilot on ONE binary** (e.g. `jit_surface`'s existing
  family-bundle tests, since they already agree on shared fixture shape) to
  validate the env-var handoff and memo-hit rate in practice, BEFORE any
  wider rollout — the experimental flag and the fixture-engineering
  precondition are both real risk, not just sizing. Do not build this
  blind; the pilot's own memo-hit measurement is the go/no-go gate.

**(d) Per-test `XDG_CACHE_HOME` isolation cost — measured, not assumed.**
`tests/support::isolate_cache()`'s own doc (quoted in
`tidepool-harness/CLAUDE.md`) already states the split: mutable session state
(checkpoints, transcripts, `log.jsonl`, KV, the GENERATED EFFECTS MODULE) is
isolated per test, but the COMPILE MEMO is shared via
`TIDEPOOL_COMPILE_CACHE_DIR` pointed at the ambient cache dir — so the memo
itself is NOT re-paid per process. What IS repeated per process: **stdlib
MATERIALIZATION and effects-module GENERATION** are filesystem writes (not
GHC compiles) that happen once per isolated `XDG_CACHE_HOME`, i.e. once per
nextest PROCESS. This measurement did not instrument that write cost
directly (it is not an `extract-shim`-visible event — it happens before any
spawn), but the repl census's own `tidepool-compile-summary modules=41
wall_ms=3111` line shows the stdlib's 41 modules type-check in ~3.1s as
PART OF a compile that already needed to happen — the materialization
(copying/writing stdlib source to a fresh dir) is a much smaller
filesystem-only cost layered before that, and sharing it across processes
(a read-only fixture) would need to defeat the isolation `isolate_cache`
deliberately provides for mutable state, which is NOT the same axis as the
compile memo. **Verdict: a real but almost certainly SMALL win (filesystem
copy, not compilation) — worth a targeted measurement in a follow-up, not
sized further here** (would need direct instrumentation of
`ensure_effects_module_at`/stdlib materialization, which this lane's
shim cannot see).

## 6. Ranked cut list

Ordered by (projected saving × confidence) / effort. Every entry is a
PROPOSAL for the operator to pick from — nothing here is built.

| # | What | Projected saving | Confidence | Effort | Risk |
|---|---|---|---|---|---|
| 1 | **Extend `jit_surface`-style family bundling to remaining eval-lane crates/binaries** (runtime's `misc` (45 tests, 1 binary — untested in this sample, flagged for follow-up), `effect_stack` (117 tests, 1 binary — largest single GHC-heavy binary in the suite, highest-leverage single target), handlers' single-assertion binaries) using `compile_targets`'s N-in-one-spawn mode (§3) rather than folding checks into one flattened expression: N assertions → 1 spawn instead of N, ~free marginal cost per extra target. | Spawn count on consolidated binaries: N→~1 per family (up to ~95% cut on THOSE binaries specifically, mirroring jit_surface's already-proven 97→42 test-count cut, extended to also cut spawn count which the test-count cut alone did not). | High (mechanism proven in §3; pattern proven in §4) | Medium — needs per-binary judgment on which checks can share a module (name collisions, differing helper imports) | Low — same all-or-nothing-spawn contract already accepted for existing family bundles; a broken check in a shared module fails the WHOLE spawn (root CLAUDE.md's known tradeoff, not new) |
| 2 | **Pilot nextest setup scripts on one already-consolidated binary** (§5c) to validate the `$NEXTEST_ENV` → `TIDEPOOL_COMPILE_CACHE_DIR` handoff end to end, before wider adoption. | Removes the FIRST-test-in-binary cold-compile tax across a binary's own tests (marginal — the memo already serves 2nd+ identical compiles); real value is validating the mechanism for future binaries, not a big standalone number. | Medium (mechanism works per docs; unproven end to end in this repo) | Low (one pilot, config-gated behind `experimental`) | Medium — experimental nextest feature; config change is out of THIS lane's boundary, needs its own follow-up lane |
| 3 | **Nothing to do for session-scoped turn compiles** (harness/repl turn lanes, 63–71% of their sampled spawns) via test consolidation — call this out explicitly rather than let it silently eat the 90% target. Making the session lane cacheable (widening the compile-memo's argv allowlist to admit `--session-root`/`--inject-val` under some narrower "same session generation, same content" key) is the ONLY lever besides reducing turn COUNT per test. | If solved: could recover most of the 41% session-scoped share of sampled wall time (296.7s of 712.0s sampled). If NOT solved: session-scoped compiles remain an unavoidable per-turn tax regardless of any test restructuring. | N/A — sizing only, not proposing to build | **Sizing:** widening the memo key to admit session state safely means keying on the INJECTED VAL MODULE'S CONTENT (not its path — the same content-addressed, path-independent trick already used for
`--include` dirs, per `tidepool-runtime/CLAUDE.md`'s "Compile cache" section)
rather than the session-root PATH. This is a real design task (a session's `Val.G<g>` content changes every generation, so the key would need to hash the actual injected module bytes, not treat the flag as categorically uncacheable) — roughly comparable scope to the ORIGINAL compile-memo lane itself (a full plan doc, a keying decision, adversarial tests). NOT a quick win; explicitly flagged as the standing hazard per the boundary's "explicit call" requirement. |
| 4 | **Reduce turns-per-test in multi-turn harness acceptance tests** (`acceptance_cross_turn`'s pattern: 10 tests, 49 spawns, ~5/test) where the assertion under test doesn't actually need EVERY intermediate turn to be a full separate spawn — e.g. combining a decl-then-use sequence into fewer turns where the property being tested survives. | Test-specific; not sized in aggregate (needs per-test judgment on whether turn count is incidental or essential to the property under test — several of these tests are EXPLICITLY testing turn-to-turn behavior, so reducing turns would test something else). | Low confidence as a blanket rule | High (needs reading each test's actual intent) | High — risks silently changing what a cross-turn test proves; flagged as LOWEST priority for exactly this reason |
| 5 | **Setup-script-driven stdlib/effects-module SHARING across processes** (§5d) — a read-only fixture for stdlib materialization if it's shown to be a real cost. | Small, unmeasured (§5d) | Low (not directly measured) | Low once measured | Low — filesystem-only, doesn't touch compile correctness |

## Total projection

**On the batchable slice (eval-lane, non-session-scoped, fixed-template
compiles):** items #1+#2 target the 59% of sampled wall time that is NOT
session-scoped (415.3s of 712.0s sampled extract wall) plus the process
overhead nextest's per-test model imposes on that same slice. The
`jit_surface`-precedent (97→42 test count, ~57% cut, achieved WITHOUT yet
touching spawn count) plus extending to spawn-count reduction (§3's
near-zero marginal cost) suggests **a 70–90% cut is realistic on this
slice specifically** — consistent with the operator's ~90% target, but only
because the target implicitly excludes what can't move.

**On the whole suite:** the session-scoped slice (41% of sampled wall time,
and structurally the majority of harness/repl turn-lane tests, which are a
large fraction of the 1057 total GHC-heavy tests) is NOT addressed by any
form of test consolidation — per the boundary's own framing, confirmed by
this measurement. **Explicit call: session-compile uncacheability DOES
dominate for the harness/repl turn-lane crates** (63–71% of sampled spawns
there), and does NOT dominate for the runtime eval-lane crate (0% in the
`jit_surface` sample). Blending these, a whole-suite projection of
**~50–60% total wall-time reduction** is a more honest number than 90% —
achievable via items #1+#2 on the eval-lane majority of tests (714 of 1057
GHC-heavy tests are in tidepool-runtime, which is 0% session-scoped in the
one binary sampled), with the harness/repl turn-lane remainder needing item
#3 (a real, separately-scoped design lane) to move further.

## What changed (for the VERIFY gate)

- `plans/test-time-cut.md` (this file) — the only tracked-repo change.
- Cherry-picked `87611006` (ghc-heavy cap 2→4, already committed and
  operator-approved on another branch) onto this branch so sampling ran at
  the current cap — this touches `.config/nextest.toml` and `CLAUDE.md`, both
  ALREADY-APPROVED content from elsewhere, not a new config decision made in
  this lane.
- No test file was modified. The measurement shim (`extract-shim.sh`) and its
  logs live entirely under this session's scratchpad directory, never
  `scripts/`, never committed.
