# Wave-3 render+loop fusion — latency receipt

## The instrument, named

`scripts/bench-turn.sh` did not exist in this tree when this measurement was
taken (a sibling dev was building it), so this is a hand measurement with the
method stated.

**It landed later, and it would NOT have served as this item's instrument
anyway — checked, not assumed.** `bench-turn.sh` has five rows
(`oneshot_cold`, `oneshot_warm`, `session`, `block5`, `harness`) and **none of
them is on wave-3's path**: neither the script nor any of its four example
bins references `SelfHarnessDriver`, `run_one_cycle`, or `render_framing` at
all. Its `harness` row drives an *answerer* `Harness` turn
(`drive_turn`/`run_to_hole_or_done`) via `ReplayProvider` — a different path
from the self-harness driver's pre-model boot cycle, which is what render+loop
fusion changes.

So this is a standing gap in that instrument, worth knowing for the next perf
lane: **`bench-turn.sh` has no self-harness-boot row.** Its `block5` row is
purpose-built for the batch-turns lane and its oneshot/session rows cover the
eval paths; the self-harness driver is uncovered. Adding such a row is a
reasonable follow-up, and `acceptance_boot_compile_count` is the instrument to
build it from.

**No bench-turn regression run was needed for the shared-template change.**
This change edits `TurnTemplate::render`, which every eval path goes through —
but the golden tests pin its output **byte-identical** when `extra_entries` is
empty, and identical bytes mean identical downstream compile cost by
construction. A timing run would be a weaker check than the byte-identity pin
it would be re-testing.

**Instrument:** `cargo-nextest`'s per-test duration line for
`tidepool-harness::acceptance_boot_compile_count::boot_pays_pre_model_extract_compiles_matching_baseline`.

That test drives the self-iterating harness from `Harness::new` through the
FIRST live model call and no further, with its compile memo isolated to a fresh
dir (`support::isolate_compile_memo`) so every extract compile on the path is
paid COLD. Its duration is therefore the pre-model boot path's wall clock —
which is exactly the quantity render+loop fusion reduces — and not a
measurement of cache state.

**Why the per-test line and not the shell wall clock.** Total invocation wall
was 107s on the first run and dominated by an 85s cold `cargo build`; the
nextest line excludes the build entirely. The build time is noise with respect
to this change, so quoting it would inflate the denominator and understate the
effect. Raw shell wall was captured for run 1 (107s) as a cross-check and then
dropped as uninformative.

The test's `PASS` is itself a second, exact receipt: it asserts the pre-model
`tidepool_extract_cmd::extract_spawn_count` against `PRE_MODEL_EXTRACT_COMPILES`,
so a passing run at the baseline is independent confirmation that the constant's
recorded value is the value this tree actually produces.

**Invocation, verbatim, identical on both sides:**

    export XDG_CACHE_HOME="$PWD/.cache"
    export TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
    scripts/battery.sh -p tidepool-harness -E 'binary(acceptance_boot_compile_count)'

**A/B method:** across commits, not `git stash` and not a reverse-applied
patch — the "before" was taken on a tree whose only commit past the wave base
is a plans-only doc commit, so the baseline sha IS the unmodified code. N=3 per
side, every run reported individually.

## BEFORE — `35b6e22a` (plans-only commit on top of `fc94ac3b`; no code delta)

`PRE_MODEL_EXTRACT_COMPILES = 2` (two `compile_outer` invocations: the pre-loop
render framing, and the loop body).

| run | test duration | result |
|---|---|---|
| 1 | 21.613s | PASS (1 passed, 0 skipped) |
| 2 | 27.740s | PASS (1 passed, 0 skipped) |
| 3 | 25.660s | PASS (1 passed, 0 skipped) |

**median 25.660s**, range 21.613–27.740s.

**Carry the spread with the number.** A ~6s range on a ~25s measurement is
~24% — this box is shared and GHC-slotted, so a single-run delta smaller than
that spread is not a result. Any claimed after/before difference must clear the
noise floor, or be reported as "not separable from run-to-run variance".

## The first A/B was INVALID, and why — read this before quoting any number

The first after-set, taken immediately after folding `wave3-fusion`, read
**26.922 / 17.393 / 14.144 s** — monotonically decreasing. Runs 4–7 on the same
unchanged tree then read **11.669 / 11.558 / 11.671 / 11.658 s**: a 0.11s
spread, ~100× tighter than the first three.

That is a warmup/contention ramp, not the change. And it invalidates the
comparison in the dangerous direction: the BEFORE set (§ below, "cold") was
taken under exactly those cold conditions, so pairing a cold before against a
warm after would have credited the fusion with the warmup as well. Median
before 25.660s vs median after 17.393s would have been quoted as a 32% win.
**It is not a result** — the two ranges overlapped (one after-run, 26.9s, was
slower than the before median), and the conditions were not matched.

The fix was to re-take the baseline under MATCHED warm conditions on the same
box in the same session, by reverse-applying the change as a patch:

    git diff b1a43900 HEAD -- tidepool-harness/src tidepool-harness/tests \
        tidepool-mcp/src > wave3.patch
    git apply -R wave3.patch     # tree is baseline CODE, plans intact
    <5 runs>
    git apply wave3.patch        # restored; `git status` clean

No `git stash` at any point. The restore was confirmed with `git status --short`
(empty) and `git log` (HEAD unmoved at `d5d9809d`).

## BEFORE — cold, first pass, SUPERSEDED (kept because it is what nearly shipped)

`35b6e22a`, `PRE_MODEL_EXTRACT_COMPILES = 2`:
21.613 / 27.740 / 25.660 s, median 25.660s. Discarded as unmatched, per above.

## BEFORE — warm, matched conditions (reverse-applied patch)

`PRE_MODEL_EXTRACT_COMPILES = 2` — two `compile_outer` spawns (pre-loop render
framing, loop body).

| run | test duration | result |
|---|---|---|
| 1 | 20.450s | PASS |
| 2 | 21.060s | PASS |
| 3 | 24.765s | PASS |
| 4 | 22.449s | PASS |
| 5 | 22.771s | PASS |

**median 22.449s**, range 20.450–24.765s (4.3s spread).

## AFTER — `d5d9809d`, warm, matched conditions

`PRE_MODEL_EXTRACT_COMPILES = 1` — one fused `compile_turns` spawn emitting
both entries.

| run | test duration | result |
|---|---|---|
| 4 | 11.669s | PASS |
| 5 | 11.558s | PASS |
| 6 | 11.671s | PASS |
| 7 | 11.658s | PASS |

**median 11.664s**, range 11.558–11.671s (0.11s spread).

(Runs 1–3 of this set — 26.922 / 17.393 / 14.144 — are the warmup ramp
documented above, excluded from the claim and reported here rather than
deleted.)

## Result

| | before | after |
|---|---|---|
| pre-model extract spawns | **2** | **1** |
| pre-model boot path, median | **22.449s** | **11.664s** |
| range | 20.450–24.765s | 11.558–11.671s |

**−10.785s, −48.0% on the median.** The distributions do **not** overlap: the
slowest after-run (11.671s) is faster than the fastest before-run (20.450s) by
8.8s. This clears the noise floor by a wide margin, which the first (invalid)
A/B did not.

The count receipt is the exact one and needs no statistics: the pre-model path
pays **one** `tidepool-extract` spawn where it paid two, asserted by
`acceptance_boot_compile_count` against `PRE_MODEL_EXTRACT_COMPILES`, which the
dev set to the value it observed.

## The mechanism — and it is NOT "half the work"

The fused invocation still compiles BOTH entries: GHC typechecks, desugars and
`core2core`s the same two fragments either way. Yet the wall time roughly
halved. So the saving is **not** per-target translation work — it is one entire
GHC session: startup, `getSessionDynFlags`/`setSessionDynFlags`, `depanal`, and
the `load'` that parses + typechecks + `core2core`s every home module, all of
which are paid ONCE PER INVOCATION regardless of how many targets that
invocation emits.

That is consistent with the wave's own Phase-B breakdown (26–32% of a spawn is
session boot, and `load'` sits inside the `ghc_session` bracket), and it is the
reason the second target is nearly free: the marginal cost of adding an entry
to an existing invocation is its own translate, not another session.

The variance figures say the same thing from the other side. The before set's
spread is 4.3s and the after set's is 0.11s — two GHC sessions give contention
two chances to land, one gives it one.

## Scope of the claim — do not widen it

This is the **pre-model boot path**: launch → first model call, first cycle,
cold compile memo. It is not "half the turn latency". Specifically:

- The POST-loop render still compiles separately and is unaffected — it uses
  the NEW state and cannot fuse. It is also after the first model call, so it
  is outside this measurement by construction.
- Once the loop is running, a cycle's cost is dominated by model calls, not by
  these compiles.
- With a WARM compile memo the whole path pays 0 spawns and this delta is 0.
  The measurement isolates the memo deliberately (`isolate_compile_memo`) so it
  reports the boot path rather than cache state.

## Rebase note — the measurement predates a rebase, and it was re-verified

Every number above was taken before this branch was rebased onto its parent
(`harness-interaction-surface`). The shas quoted are the post-rebase ones; the
code content is identical (the rebase applied cleanly with no conflicts).

That is not on its own sufficient, because the parent's `e50b96c1`
(`fix(mcp): uses_qq drops the deleted [form| token`) touches
**`tidepool-mcp/src/eval_prep.rs` — the same file this change edits.** A clean
rebase means git found no textual conflict, not that the two changes are
semantically compatible. So the legs below were re-run at the rebased tip
rather than inherited from the pre-rebase run, and the spawn-count receipt was
re-taken there too.

**The measurement replicates there.** `acceptance_boot_compile_count` re-run
×3 at the rebased tip: **11.449 / 11.118 / 12.353 s**, median 11.449s — within
the pre-rebase after-set's band (median 11.664s) and still comfortably clear of
the 20.450s baseline floor. Every run PASSed, so the spawn count is still 1.

## Verify legs at the rebased tip (`f05433e4`)

    cargo check --workspace --all-targets            rc=0
    cargo clippy --workspace --all-targets           rc=0, no warnings emitted
    cargo fmt --all -- --check                       rc=0
    cargo nextest run                                1742 passed, 0 failed, 12 skipped
      (covers this change's eval_prep golden tests AND the parent's uses_qq
       proptest, which is the pair the shared-file rebase put at risk)
    scripts/battery.sh -p tidepool-harness \
      -E 'binary(golden_path) + binary(acceptance_askuser)'
                                                     4 passed, 0 skipped
      · acceptance_askuser::askuser_operator_form_round_trip_and_ws4_log   PASS 32.843s
      · acceptance_askuser::prd_example_adts_compile_with_the_bare_derive_contract PASS 4.283s
      · golden_path::fork_only_resumes_to_completion                       PASS 14.945s
      · golden_path::golden_path_record_replay                             PASS 14.664s
    scripts/battery.sh -p tidepool-harness \
      -E 'binary(acceptance_boot_compile_count)'     1 passed, 0 skipped
                                                     (×12 runs total across the
                                                      lane, all PASS)

Pre-rebase, the same two harness legs read 34.418 / 4.322 / 14.730 / 15.329s,
4 passed — i.e. unchanged by the rebase within run-to-run variance.

Extract-side legs (`extract-fidelity-test`, `cross_mode_targeted`) were **not**
run and are **not** required: this change touches no Haskell and no extract
behaviour — it drives the already-landed `--targets` mode through an existing
Rust entry point. Stating that rather than running them for appearance.

The dev additionally ran, at the same tip: `acceptance_multi_target` (2 passed),
`acceptance_selfharness` (1), `selfharness_spine` (1), `dogfood_observability`
(1).
