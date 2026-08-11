# D2 — `RuntimeTypeClosure` — execution receipt

TL: `d2-exec`, 2026-08-11, branch `root.d2-exec`, base `fe1ade55`.

Executes `03-d2-handoff.md` per `04-turn-latency-plan.md` §1/§1a and the root
ruling at its end. Re-checked every handoff claim against `fe1ade55` (post
`batch-turns` fold) before implementing — see "Claims re-check" below.

---

## Claims re-check vs `fe1ade55` (batch-turns-induced staleness)

`04-turn-latency-plan.md` was written mid-wave, before `batch-turns` folded
its `--turn-batch` extract mode. Re-verified at `fe1ade55`:

- **`allMeta` assembly sites, unchanged in count and shape.** Still exactly
  two: `writeClosedTargets` (`Main.hs:560`, CHECK-A-guarded, reached by
  `writeWholeModuleClosed`, `runMultiTargetClosed`, and — newly, since §1 was
  written — `writeBatchItemOutput`'s `--turn-batch` compile path, which
  routes through `writeWholeModuleClosed` rather than assembling its own
  metadata) and `processFile`'s per-binding `(Nothing, False)` mode
  (`Main.hs:411-434`, unguarded, non-production). **batch-turns added a third
  CALLER of the guarded path, not a third assembly site** — `writeClosedTargets`
  is still the one place to narrow.
- **`prTyCons`/`tycons` still the unfiltered `mg_tcs` sweep.**
  `GhcPipeline.hs:457/677` (`allTyCons = concatMap (mg_tcs . mfDesugared)
  fronts`) — untouched by batch-turns.
- **`collectTransitiveDCons` (the binder-type closure) untouched** —
  `Translate.hs:1291` (renumbered from `~1094` at hand-off time, same body).
- **CHECK A (`assertMetaCoversEmitted`) still called from `writeClosedTargets`,
  before the write, before the encode `timeSection`** — `Main.hs:643-644`.
- **The pinned id-stability trio still exists by the paths `04-...md`
  enumerates** (re-run below, by name).

No batch-turns-induced staleness found beyond the new caller (harmless — it
was already routing through the guarded path, not adding a new one).

---

## The ruling, applied

Root ruling (`04-turn-latency-plan.md`, end): the "derive the five from
`tidepool_repr::freer_names`, never hand-list" requirement is unsatisfiable
across the language boundary as written; the intent-preserving form is **do
not replace or bypass the binder-type closure**, which supplies all five
roots structurally with no list on either side.

**`collectTransitiveDCons` (the binder-type closure) is untouched by this
change** — not edited, not bypassed, still called with the same
`allReachBinds` argument it always was. The only removal is `tyconMeta =
collectDataCons tycons` (the unfiltered `mg_tcs` sweep) from
`writeClosedTargets`'s merge.

---

## Step 1 (mandatory, per handoff): attribute the five freer roots empirically

Built a minimal probe (`PureProbe.hs`: `import Control.Monad.Freer; result ::
Eff '[] Int; result = pure 5`) — a genuine pure entry term, the shape the
handoff names. Ran it through `tidepool-extract-bin` directly (no fixture
regen, no live model call — just the extractor CLI on a hand-written file).

**tyconMeta never contained them, confirmed by direct inspection, not just
code-reading.** Added a temporary `TIDEPOOL_D2_DEBUG_SOURCES=1` diagnostic (working-tree only,
stripped before the committed diff) that tagged every Freer/OpenUnion/FTCQueue-
matching entry by source list. Result on `PureProbe.hs`:

    [D2-DEBUG] twUsedMeta: Control.Monad.Freer.Internal.Val
    [D2-DEBUG] siblingMeta: Control.Monad.Freer.Internal.Val
    [D2-DEBUG] siblingMeta: Control.Monad.Freer.Internal.E
    [D2-DEBUG] transitiveMeta: Control.Monad.Freer.Internal.Val
    [D2-DEBUG] transitiveMeta: Control.Monad.Freer.Internal.E
    [D2-DEBUG] transitiveMeta: Data.OpenUnion.Internal.Union
    [D2-DEBUG] transitiveMeta: Data.FTCQueue.Leaf
    [D2-DEBUG] transitiveMeta: Data.FTCQueue.Node

`collectDataCons _tycons` (the old `tyconMeta`) produced ZERO matching rows —
confirming, by direct observation rather than code-reading alone, that the
home-`mg_tcs` sweep never supplied the five (freer-simple/open-union/FTCQueue
are external packages, never in any home module's `mg_tcs`). `Val` is
supplied by BOTH `twUsedMeta` (it's Core-constructed on a pure entry term,
matching the handoff's own claim) and `transitiveMeta`; `E`/`Union` are
supplied ONLY by `transitiveMeta` (never Core-constructed on a pure term,
exactly the mandatory-roots hazard); `Leaf`/`Node` likewise only via
`transitiveMeta`.

**A/B confirms no regression.** Diff-to-patch (`git apply -R` /
`git apply`), same probe, same box: baseline (`fe1ade55`, pre-D2) and D2
produce byte-IDENTICAL freer-root coverage — same five qualified names
present, same `.Internal.`/non-`.Internal.` spellings, on both sides. D2 does
not change what supplies the five; it only removes the source that never
supplied them.

### A separate, pre-existing finding (out of this change's scope, flagged for root)

The qualified names this toolchain actually emits are
`Control.Monad.Freer.Internal.Val`/`.E` and `Data.OpenUnion.Internal.Union`
(confirmed on BOTH the pre-D2 baseline and the D2 tree — not introduced by
this change). `tidepool-repr/src/freer_names.rs`'s `VAL_QUALIFIED`/`E_QUALIFIED`/
`UNION_QUALIFIED` constants are hardcoded as `"Control.Monad.Freer.Val"` /
`"Control.Monad.Freer.E"` / `"Data.OpenUnion.Union"` — WITHOUT `.Internal.`.
`freer_names::resolve` falls back to bare-name lookup
(`table.get_by_name(bare)`) when the qualified lookup misses, so this drifts
silently today (the bare names `Val`/`E`/`Union`/`Leaf`/`Node` are unlikely to
collide in practice) — but it means the qualified-first collision defense
`freer_names.rs`'s own docstring describes (the `Data.Tree.Node` example) is
currently DEAD CODE for these five: the qualified lookup can never hit, so a
real `Node`/`Leaf`/`Val`/`Union`/`E` collision in a user eval would fall
through to `get_by_name`, which returns `None` on ambiguity — silently
failing to resolve rather than resolving via the qualified spelling as
designed. Outside this change's file surface (`tidepool-repr`, not
`haskell/`) — flagged for root/`tidepool-repr`'s owner rather than fixed
here.

---

## The implementation

`haskell/app/Main.hs`'s `writeClosedTargets` (`Main.hs:560`): removed
`tyconMeta = collectDataCons tycons` (renamed the now-unused parameter to
`_tycons` — `processFile`'s own per-binding mode still calls
`collectDataCons` directly, so the function stays exported). Added
`Tidepool.Translate.siblingCloseDCons` (`Translate.hs`, next to
`tyConToDCMeta`): for every DataCon actually built/matched in reachable Core
(`cmUsedDCs`, threaded through a new `twUsedDCs` field on `TargetWrite`),
include every OTHER constructor of that DataCon's parent TyCon too (skipping
GHC-compiler TyCons, mirroring `closeTyCons`'s own guard) — the
sibling-completeness half of `RuntimeTypeClosure` that `collectTransitiveDCons`
already provides for binder-type-reachable TyCons but that Core-construction-only
reachable TyCons lacked. New merge:

    [ wiredInMeta, concatMap twUsedMeta writes, siblingMeta, transitiveMeta ]

(previously `[ wiredInMeta, tyconMeta, concatMap twUsedMeta writes, transitiveMeta ]`).

`processFile`'s unguarded per-binding mode is deliberately left untouched —
it has no CHECK A, no `cmReachBinds`, and is not on any production path
(the harness always passes `--target`/`--targets`, never bare); narrowing it
has no detector, so it's out of scope here (documented inline at the merge
site, per `04-...md`'s instruction to state the reason rather than narrow
silently).

---

## Measured before/after — warm-matched, diff-to-patch (wave-3 discipline)

**What NOT to size against:** `assertMetaCoversEmitted` (CHECK A) is UNTIMED
— per standing hazard 6, a `translate`-phase delta would not attribute to
this change. Sized instead against `meta.cbor` byte/entry counts (exact,
deterministic — a count receipt, not a timing one) and the `cbor_encode`
phase line.

**Instrument.** Direct `tidepool-extract-bin` invocation (not through the
runtime's compile memo — this is a raw CLI compile either way, so no memo
pinning needed) on a fixture shaped like a real eval: `import Tidepool.Prelude
hiding (error)` (pulling in the real stdlib source tree as home modules,
matching what `mg_tcs` actually sees for a production eval) plus a two-line
body, compiled `--target result` (single target — the production harness's
own path, per `Main.hs`'s comment that `compile.rs` always passes `--target`,
never `--all-closed`).

**Method:** `git diff fe1ade55 -- haskell/app/Main.hs
haskell/src/Tidepool/Translate.hs > d2.patch`; measured AFTER on the tree as
committed; `git apply -R d2.patch` (never `git stash`); rebuilt; measured
BEFORE; `git apply d2.patch`; rebuilt; confirmed `git status` clean and HEAD
unmoved. `TIDEPOOL_TIMING=1`, N=3 per side.

| | BEFORE (`fe1ade55`) | AFTER (D2) |
|---|---|---|
| meta.cbor entries | **109** (×3 runs, identical) | **57** (×3 runs, identical) |
| meta.cbor bytes | **7953** (×3 runs, identical) | **3502** (×3 runs, identical) |
| `cbor_encode` phase | 0ms (×3 each side) | 0ms (×3 each side) |

**−47.7% entries, −56.0% bytes** on this fixture. `cbor_encode`'s own phase
line is too fast to resolve at this fixture's scale (sub-millisecond either
side) — an honest report, not a gap: the wave's own hazard 6 note already
flagged that D2's win is size, not necessarily a resolvable phase-line delta
on a small fixture. **This fixture is much smaller than the wave's own
"real session Core" (164/166 constructors)** — carry that with the number:
this receipt's ratio (109:57, ~1.9:1) is NOT the wave's headline 6.8:1/11.1:1
figure and must not be conflated with it. Extraction succeeded (CHECK A
passed silently) on both sides at every run.

---

## Full verify (base `fe1ade55`, this branch `root.d2-exec`)

    cargo check --workspace --all-targets              rc=0
    cargo clippy --workspace --all-targets              rc=0, no warnings from this change
                                                         (pre-existing warnings in Main.hs/Translate.hs
                                                          unchanged — isExternalName, GHC.Types.Unique,
                                                          GHC.Types.Var, mapBang/valueRepArity unused-import,
                                                          `result` name-shadowing — none introduced by D2)
    cargo fmt --all -- --check                          rc=0
    cargo nextest run                                   1743 passed, 0 failed, 12 skipped

    cd haskell && cabal build tidepool-extract-bin       builds clean
    cabal test extract-fidelity-test                    40/40 checks passed, incl. the D1 mutation
                                                          test (knob SET fails extraction with CHECK
                                                          A's exact message; knob UNSET control passes)
    cabal test session-c-test                            PASS (useX/useXe session-binder GO, round-trip GO)

    scripts/battery.sh -p tidepool-runtime \
      -E 'binary(cross_mode_targeted)'                   10/10 passed (incl. b1/b2 name-collision
                                                          dimensions, directly on point for a
                                                          sibling-set change)

    export XDG_CACHE_HOME="$PWD/.cache"
    scripts/battery.sh -p tidepool-harness \
      -E 'binary(golden_path) + binary(acceptance_boot_compile_count)'
                                                         3/3 passed:
      · acceptance_boot_compile_count::boot_pays_pre_model_extract_compiles_matching_baseline  PASS 10.229s
      · golden_path::fork_only_resumes_to_completion                                            PASS 13.792s
      · golden_path::golden_path_record_replay                                                  PASS 13.999s

**Base carries boot's ConTags pin** (per `03-d2-handoff.md`'s fold-ordering
constraint): `acceptance_boot_compile_count` — the guard test itself — ran
and passed above, on base `fe1ade55`, named by test path, not inferred from a
pass count over an unnamed suite.

**The pinned id-stability trio, run individually and by name** (per
`03-d2-handoff.md` §3 — D2 is the item they actually guard):

    tidepool-repr::extend_checked_equivalence::distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order   PASS
    tidepool-repr::extend_checked_equivalence::merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard   PASS
    tidepool-runtime::session_table_qualified_identity::accumulated_session_table_keeps_one_id_per_qualified_name       PASS

None fired red — a reachability-narrowed `DataConTable` did not drop a
constructor or collide a qualified name under any of the three guards.

---

## Wire status

**Moves.** `meta.cbor`'s byte content changes for any compile that goes
through `writeClosedTargets` with a narrower `tyconMeta`-free table (entry
count and bytes differ, per the measurement above). Flagged for root's
redeploy set, per the standing constraint ("D1-A did not move it; C1 did not;
E6 did").

---

## Not in scope, left for root/other lanes

1. `tidepool-repr/src/freer_names.rs`'s qualified-name constants not matching
   this toolchain's actual `.Internal.` module spellings (see "A separate,
   pre-existing finding" above) — a real, silent gap in the documented
   qualified-first collision defense, unrelated to and unaffected by D2.
2. `processFile`'s unguarded per-binding mode still calls the unfiltered
   `collectDataCons` — deliberately left narrow-free (no CHECK A to catch a
   mistake there, not on any production path).
