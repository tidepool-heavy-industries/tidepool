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

Two new counters, both permanent:

- `CodegenPipeline::blocks_emitted` (`tidepool-codegen/src/pipeline.rs`) —
  session-lifetime count of Cranelift IR blocks across every successfully
  compiled function, incremented in `define_function` alongside
  `functions_defined`. Public getter `blocks_emitted()` mirrors
  `functions_defined()`.
- A `fragment_stats` `log::debug!` line (target `tidepool::codegen`) in
  `compile_expr` (`tidepool-codegen/src/emit/expr.rs`), emitted once per
  `add_function` call, one pass over the tree's flat node vector:
  `core_cons` (distinct `DataConId`s referenced — both `Con` construction
  sites and `Case`/`AltCon::DataAlt` discriminators), `app_nodes`,
  `con_nodes`, `lam_nodes`, `case_nodes`, `nodes`. It correlates with
  `jit_machine.rs`'s existing `add_function` line by `name=`; it is a
  separate line rather than a field added to that one because `jit_machine.rs`
  carries a sibling agent's uncommitted work and was off-limits for this task.

## The measurement point matters: `core_cons` is read from the WRAPPED tree

`compile_expr`'s `tree` parameter is the tree **after** `wrap_with_datacon_env`
has already run (`jit_machine.rs` calls `wrap_with_datacon_env` first, then
passes the result to `compile_expr`). On the current (reverted) base, that
wrapper step unconditionally prepends one `LetNonRec` binding per constructor
in the session table — so every table constructor is mechanically referenced
in the tree `fragment_stats` walks, regardless of whether the fragment's own
Core ever uses it. This is visible directly in the numbers below:
`core_cons == table_cons == wrapped_cons`, exactly, on every fragment
captured. That equality is an artifact of measuring downstream of the
wrapper, not evidence that the fragment's own Core references that many
constructors.

**This is the one number the other team is waiting on, and I could not get
it on this base.** Getting the fragment's true reachable-constructor count
requires walking the tree *before* `wrap_with_datacon_env` runs — a
measurement point that lives at the `jit_machine.rs` call site (the only
place both the pre-wrap tree and the table are in scope together), which was
off-limits for this task. Marking this absent rather than reporting the
1:1 `core_cons`/`table_cons` equality as if it answered the question — it
does not.

## Real numbers — `resident_session` (`multi_turn_accumulates_across_suspend_resume`, real GHC-extracted session)

Captured via `RUST_LOG=tidepool::codegen=debug` through
`scripts/ghc-slots.sh run -- cargo nextest run --ignore-default-filter -p
tidepool-runtime -E 'binary(resident_session)' --no-fail-fast
--success-output final --failure-output final`, shared prebuilt
`TIDEPOOL_EXTRACT`. Same session shape as the `11-...` receipt (164→166
constructor table across turns).

| add_function | table_cons | core_cons | wrapped_cons | funcs | blocks | app_nodes | nodes |
|---|---|---|---|---|---|---|---|
| turn1_1 | 164 | 164 | 164 | 232 | **not captured** | 345 | 3748 |
| turn2_2 | 166 | 166 | 166 | 25 | **not captured** | 13 | 787 |

Additional context from the unmodified `jit_machine.rs` `add_function` line
(pre-existing instrumentation, unchanged by this task): turn1 `dce_calls=224
dce_nodes=568211 dce_ms=3302.230`; turn2 `dce_calls=166 dce_nodes=76788
dce_ms=392.758`. Both track table size (164, 166), not fragment size — the
same pattern the `11-...` receipt described as "fixed," now visible again
because the fix that computed it once instead of once-per-level is the
commit that got reverted.

`blocks` is **not captured** for either row. `blocks_emitted()` is a
session-lifetime cumulative counter (same contract as `functions_defined`);
reading a correct per-`add_function` delta needs a before/after snapshot at
the `jit_machine.rs` call site, the same place `funcs` and `dce_delta` are
already read there — a site this task could not touch. The counter is landed
and ready for that fold-in; this report does not fill the cell from
inference.

## Fixture (`bind_error_then_allocate`) — not captured

Campaign quiesce arrived before this step started. Per the quiesce directive
("if it has not started, do not start it now — report what you have and
what is missing"), this data point is not in this report. The vehicle
(`RUST_LOG=tidepool::codegen=debug` through the same `ghc-slots.sh run --`
pattern, aimed at whatever binary hosts `bind_error_then_allocate` — see the
`11-...` receipt for the original fixture location) is unchanged from what
this task already used successfully for `resident_session`; a follow-up run
should be a direct rerun of that pattern.

## Prose answers

**Ratio of metadata constructors to reachable-Core constructors.** Not
obtainable from this instrumentation on this base — see "The measurement
point matters" above. What is measurable, `table_cons` vs `core_cons`
*as read from the post-wrap tree*, is 1:1 on both captured turns (164/164,
166/166), because the un-pruned wrapper spine mechanically inserts a
reference to every table constructor ahead of where this counter reads.
That is not the ratio the other team asked for, and reporting it as such
would be exactly the "number someone trusts and acts on that turns out to
be inferred rather than measured" the quiesce note warned against. Getting
the real number needs a pre-wrap measurement point, which needs a
`jit_machine.rs` edit — out of this task's scope. Flagging this as open
work rather than inventing a workaround.

**Cranelift functions and App nodes per turn.** Turn 1 (164-constructor
table, 3748-node post-wrap tree, 345 `App` nodes) compiled 232 Cranelift
functions. Turn 2 (166-constructor table, 787-node tree, 13 `App` nodes)
compiled 25. Functions-per-App-node is not a fixed ratio between the two
turns (0.67 vs 1.9) — consistent with turn 1 doing substantially more
first-time lambda/thunk/join-point compilation work than turn 2's smaller,
narrower fragment.

**Does per-turn cost track table size or fragment size?** Table size:
164 → 166 (+1.2%). Fragment size by every structural measure shrank
sharply: `app_nodes` 345 → 13 (−96%), `funcs` 232 → 25 (−89%), `nodes`
3748 → 787 (−79%). But `wrapped_cons` and `dce_calls` track the table
size exactly (164, then 166) on both turns, not the fragment shrinkage.
This is the un-pruned chain doing exactly what the `11-...` receipt
predicted it would do if the prune were absent: wrapper construction and
the dead-code elimination that has to clean it back up both scale with
*accumulated session table size*, and pay that cost again on every single
turn regardless of how small that turn's own fragment is.

## Quick tier — honest state, not a clean gate

Box-contended throughout this task (many concurrent sibling-agent
cargo/nextest processes). Bare `cargo nextest run` fail-fast-stopped at
**1050/1737 tests run: 1046 passed (26 slow), 4 failed, 10 skipped** (687
not run — fail-fast, not a real skip). The 4 failures were
`tidepool-codegen::proptest_primops_differential` {`prop_word_binary`,
`prop_float_binary`, `prop_int_binary`, `prop_double_binary`}, each aborting
on `[FIXTURE WATCHDOG] ... exceeded 120s — suspected non-termination` — not
the known `selfharness_compaction` garbage-constructor-tag signature.
Re-run in isolation with less box contention
(`-E 'test(prop_word_binary) or test(prop_float_binary) or
test(prop_int_binary) or test(prop_double_binary)'`): **4/4 passed, 25–43s
each**, well inside the 120s watchdog. That's strong evidence the fail-fast
failures were box-contention-induced CPU starvation, not a regression from
the `wrap_with_datacon_env` revert or this task's own changes — but it is
not a full re-run of the suite, and a complete `--no-fail-fast` count for
the whole quick tier was **not obtained** (would need another 20–30+ minutes
under contention; scope closed before that ran).

`cargo check --workspace --all-targets`: clean (confirmed after the
cherry-picked revert landed). `cargo fmt --all -- --check`: clean. `cargo
clippy -p tidepool-codegen --all-targets`: no new warnings — the one warning
present (`large_enum_variant` on `ResponsePlan`, `jit_machine.rs:2288`)
predates this work and is noted as such in the `11-...` receipt.

## Other sightings (not chased, per scope)

- `resident_session::nested_child_runs_while_parent_suspended_then_resumes`
  FAILED: `resume after children: Run(Jit(HeapBridge(NurseryExhausted)))`.
  Different signature from the known `selfharness_compaction`
  garbage-constructor-tag bug — flagging per "any failing test is new,"
  not investigated (out of this task's scope).
- No sighting of the known `selfharness_compaction`
  "unexpected constructor tag: <enormous number>" fast-abort in any suite
  this task ran.

## Base note

This branch cherry-picked `1345296b` (`root.jit-chain`'s revert of
`cb1b131d`) on top of `92961413` so its own measurements reflect the current
base rather than a stale pruned one. `datacon_env.rs` is otherwise
untouched by this task; only `pipeline.rs` and `emit/expr.rs` carry new code.
