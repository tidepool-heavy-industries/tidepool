# Wave-3 render+loop fusion — latency receipt

## The instrument, named

`scripts/bench-turn.sh` did not exist in this tree when this measurement was
taken (a sibling dev is building it), so this is a hand measurement with the
method stated.

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

## BEFORE — `1cdbc013` (plans-only commit on top of `fc94ac3b`; no code delta)

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

## AFTER

Pending the fold of `wave3-fusion`.
