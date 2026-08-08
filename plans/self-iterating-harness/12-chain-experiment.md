# Chain experiment — fragment-side codegen counters

Companion to `11-jit-codegen-latency-receipt.md`. That receipt's numbers were
measured with `cb1b131d` ("wrap_with_datacon_env binds only referenced
constructors") in place, i.e. with the datacon-env wrapper spine pruned to the
constructors a fragment actually references. **`cb1b131d` has since been
reverted** (miscompile on a second fragment compiled against an
already-populated session table). This report is measured on the reverted
base: wrappers are built for every constructor in the accumulated session
table again, on every fragment, unconditionally. That is expected, correct
behavior on this base, not a finding of this experiment.

## What landed

Three counters, all permanent and all gated on `log::log_enabled!(target:
"tidepool::codegen", log::Level::Debug)` so a disabled target costs one
atomic load, not the walk it would otherwise pay for on every compile:

- `CodegenPipeline::blocks_emitted` (`tidepool-codegen/src/pipeline.rs`) —
  session-lifetime count of Cranelift IR blocks across every successfully
  compiled function, incremented in `define_function` alongside
  `functions_defined`. Public getter `blocks_emitted()` mirrors
  `functions_defined()`. Read via the same before/after snapshot as
  `functions_defined` in `add_function` (`jit_machine.rs`), so `blocks=` is
  now a genuine per-`add_function` delta in that log line, not an
  unread field.
- `core_cons_prewrap` (`jit_machine.rs::add_function`) — distinct
  `DataConId`s referenced in the fragment's own Core, read on the
  **post-normalize, pre-wrap** tree (after `tidepool_repr::normalize`,
  before `wrap_with_datacon_env`). This is the number the metadata-vs-
  reachable ratio needs, and it required editing `jit_machine.rs`: the
  boundary that blocked that edit was lifted mid-task once the sibling
  agent with uncommitted work there had folded.
- `fragment_stats` (`tidepool-codegen/src/emit/expr.rs::compile_expr`) —
  `core_cons`, `app_nodes`, `con_nodes`, `lam_nodes`, `case_nodes`, `nodes`
  on the **post-wrap** tree `compile_expr` actually receives (the tree
  handed to codegen). Kept as a separate log line from `add_function`'s,
  correlated by `name=`, because it measures a genuinely different tree
  than `core_cons_prewrap` does — not because of any file-access
  restriction (that restriction existed earlier in this task and no
  longer does).

Timing hygiene: `core_cons_prewrap`'s walk necessarily sits inside
`add_function`'s `shape_start`..`shape_ms` window (it needs the pre-wrap
tree, which `wrap_with_datacon_env` consumes right after). Its own elapsed
time is measured separately and subtracted out of `shape_ms`, so the
diagnostic walk does not inflate the one timing bucket this whole wave is
trying to reduce. The set-building loop also avoids the per-node `Vec`
allocation an earlier `flat_map` version had (a 3000-node tree was doing
~3000 short-lived allocations to build a ~24-element set) — a direct
insert loop, matching `fragment_stats`'s shape.

## THE RATIO — metadata constructors vs. constructors reachable from the fragment's own Core

Real numbers, real GHC-extracted session (`resident_session::
multi_turn_accumulates_across_suspend_resume`, same session shape as the
`11-...` receipt, 164→166 constructor table across turns):

| turn | table_cons (metadata) | core_cons_prewrap (reachable) | ratio |
|---|---|---|---|
| turn 1 | 164 | 24 | **6.8 : 1** |
| turn 2 | 166 | 15 | **11.1 : 1** |

**This is a real finding, not near 1:1.** The fragment's own Core reaches
only a small fraction of the accumulated session table — 24 of 164
constructors on turn 1, 15 of 166 on turn 2. It is not quite the "hundreds
to dozens" framing of the original hypothesis (the table itself is in the
hundreds only relative to a much smaller per-fragment reachable set, and
the table itself is 164–166, not hundreds), but the direction and the scale
are confirmed: single-digit-to-low-teens percent of the metadata table is
what any one fragment actually touches. This is exactly the shape of
over-collected Haskell-side metadata the wave hypothesized.

(For contrast, the previously-reported `core_cons` measured on the
*post-wrap* tree — see `fragment_stats` below — coincides with `table_cons`
exactly, 164/164 and 166/166. That number answers "what does codegen's
input tree reference," not "what does the fragment's own Core reference";
it is not the ratio above and should not be read as one.)

## Full per-`add_function` table

Captured via `RUST_LOG=tidepool::codegen=debug` through
`scripts/ghc-slots.sh run -- cargo nextest run --ignore-default-filter -p
tidepool-runtime -E 'binary(resident_session)' --no-fail-fast
--success-output final --failure-output final`, shared prebuilt
`TIDEPOOL_EXTRACT`.

| add_function | table_cons | core_cons_prewrap | core_cons (post-wrap) | wrapped_cons | funcs | blocks | app_nodes | nodes |
|---|---|---|---|---|---|---|---|---|
| turn1_1 | 164 | 24 | 164 | 164 | 232 | 13348 | 345 | 3748 |
| turn2_2 | 166 | 15 | 166 | 166 | 25 | 494 | 13 | 787 |

Additional context from `jit_machine.rs`'s `add_function` line: turn1
`dce_calls=224 dce_nodes=568211 dce_ms=1757.823`; turn2 `dce_calls=166
dce_nodes=76788 dce_ms=218.206`. Both track table size (164, 166), not
fragment size — the pattern the `11-...` receipt described as "fixed," now
visible again because the fix that computed it once instead of
once-per-level is the commit that got reverted.

## Fixture (`bind_error_then_allocate`) — not captured

Out of scope for the bounded resume that added `core_cons_prewrap` and
wired `blocks_emitted` in; explicitly not authorized. Still uncaptured from
the original task. The vehicle (`RUST_LOG=tidepool::codegen=debug` through
the same `ghc-slots.sh run --` pattern) is unchanged from what this task
already used successfully for `resident_session`; a follow-up run should be
a direct rerun of that pattern against whichever binary hosts that fixture
(see the `11-...` receipt for its original location).

## Prose answers

**Ratio of metadata constructors to reachable-Core constructors.** See
"THE RATIO" above: 6.8:1 (turn 1), 11.1:1 (turn 2). Real, measured on the
fragment's own pre-wrap Core, not inferred and not the post-wrap
coincidence-with-`table_cons` this report flagged earlier as a
measurement-point artifact.

**Cranelift functions and blocks per turn, App nodes per turn.** Turn 1
(164-constructor table, 3748-node post-wrap tree, 345 `App` nodes) compiled
232 Cranelift functions totaling 13348 blocks (~57.5 blocks/function).
Turn 2 (166-constructor table, 787-node tree, 13 `App` nodes) compiled 25
functions totaling 494 blocks (~19.8 blocks/function). Functions-per-App-node
is not fixed between the two turns (0.67 vs 1.9), consistent with turn 1
doing substantially more first-time lambda/thunk/join-point compilation
work than turn 2's smaller, narrower fragment. Blocks/function dropping by
roughly a third from turn 1 to turn 2 is consistent with turn 2 compiling
more small, single-block-ish functions relative to turn 1's larger control-
flow-heavier ones.

**Does per-turn cost track table size or fragment size?** Table size:
164 → 166 (+1.2%). Fragment size by every structural measure shrank
sharply: `app_nodes` 345 → 13 (−96%), `funcs` 232 → 25 (−89%), `blocks`
13348 → 494 (−96%), `nodes` 3748 → 787 (−79%), and now `core_cons_prewrap`
itself 24 → 15 (−38%). But `wrapped_cons` and `dce_calls` track the table
size exactly (164, then 166) on both turns, not the fragment shrinkage.
This is the un-pruned chain doing exactly what the `11-...` receipt
predicted it would do if the prune were absent: wrapper construction and
the dead-code elimination that has to clean it back up both scale with
*accumulated session table size*, and pay that cost again on every single
turn regardless of how small that turn's own fragment — or its own
reachable-constructor set — actually is.

## Quick tier — honest state, not re-run this pass

Not re-run in this resume per explicit instruction (root is running the
full quick tier at their level). From the original pass: box-contended
throughout, bare `cargo nextest run` fail-fast-stopped at **1050/1737
tests run: 1046 passed (26 slow), 4 failed, 10 skipped** (687 not run —
fail-fast, not a real skip). The 4 failures were
`tidepool-codegen::proptest_primops_differential` {`prop_word_binary`,
`prop_float_binary`, `prop_int_binary`, `prop_double_binary`}, each aborting
on `[FIXTURE WATCHDOG] ... exceeded 120s — suspected non-termination` — not
the known `selfharness_compaction` garbage-constructor-tag signature.
Re-run in isolation with less box contention: **4/4 passed, 25–43s each**,
well inside the 120s watchdog — strong evidence the fail-fast failures were
box-contention-induced CPU starvation, not a regression from the
`wrap_with_datacon_env` revert or this task's own changes.

`cargo check --workspace --all-targets`: clean (confirmed after this
resume's `jit_machine.rs` edits). `cargo fmt --all -- --check`: clean.
`cargo clippy -p tidepool-codegen --all-targets`: no new warnings — the one
warning present (`large_enum_variant` on `ResponsePlan`, now
`jit_machine.rs:2320` after this resume's line-number shift) predates this
work and is noted as such in the `11-...` receipt.

## Other sightings (not chased, per scope)

- `resident_session::nested_child_runs_while_parent_suspended_then_resumes`
  FAILED again in this resume's capture run:
  `resume after children: Run(Jit(HeapBridge(NurseryExhausted)))`. Same
  sighting as the original pass, reported and already being escalated
  by root.
- No sighting of the known `selfharness_compaction`
  "unexpected constructor tag: <enormous number>" fast-abort in any suite
  this task ran.

## Base note

This branch cherry-picked `1345296b` (`root.jit-chain`'s revert of
`cb1b131d`) and rebased onto `root.jit-chain`'s tip (`5c35d2f1`) so its
measurements reflect the current base. `datacon_env.rs` is untouched by
this task. `pipeline.rs`, `emit/expr.rs`, and — as of this bounded resume —
`jit_machine.rs` (`add_function` only) carry the new instrumentation.
