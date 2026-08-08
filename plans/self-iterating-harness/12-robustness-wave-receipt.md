# Robustness wave — receipt (F3, F4, F6, F7, crash recovery, quality sweep)

Final SHA `96f584dd`. Criteria of record: `10-external-review-findings.md`;
specs: `11-robustness-wave-specs.md`.

## What landed

- **F3 lifecycle** (`4a8c9b95`, `650b85a7`) — `SelfHarnessState::Failed`/
  `Poisoned`. An errored cycle discards the answerer, its framing, the cycle
  compaction, the inference counter and `self.outer` (which may be parked
  mid-fragment on a hole) before publishing `Failed`, so the next cycle
  re-bootstraps. A bootstrap failure *while recovering* escalates to
  `Poisoned`, which `run_one_cycle`/`run_loop`/`restore` refuse.

  Closed by a **mutation check**, not a green run: removing `self.outer = None`
  turns the recovery test red with `session is suspended on continuation
  scont_1`; restoring the unconditional `Idle` turns it red with `an errored
  cycle must publish Failed`. Both reverts left the tree byte-identical.
  Verifying also found a gap the original landing missed — a *fresh* driver's
  bootstrap failure still published the cosmetic `Idle`.

- **F4 checkpoint** (`df4ab614`) — one atomically-written
  `Checkpoint{generation, state, compaction, harness_source}`, committed at
  `run_one_cycle`'s success tail. Not `run_loop`: that was an error in the
  spec, since the acceptance path drives `run_one_cycle` directly and a
  `run_loop`-sited commit leaves every acceptance-driven cycle non-durable. A
  mid-loop compaction updates memory only and never commits alone, so a crash
  mid-loop restores generation N's state *and* generation N's summary. The
  `state.json`/`compaction.txt` pair and helpers are deleted, not kept as a
  fallback.

- **F6/F7** (`e999a2b7`) — `flush_effects` returns `Result`, restores unwritten
  records ahead of anything concurrently pushed, never advances `effect_seq`
  past a failed append; all seven call sites propagate. A per-node RAII
  `TurnLease` covers snapshot → provider await → log append → resident run →
  outcome publish, acquired at exactly one layer (`drive_turn`,
  `summarize_turn`, each `answer_*`); `run_to_hole_or_done` and `follow_up`
  loop `drive_turn` without acquiring, and `drive_answerer_to_value` reaches
  `stream_turn` directly, so no call chain acquires twice on one node.

- **Crash recovery** (`87c1de8f`) — spawns the real `tidepool-selfharness` via
  `CARGO_BIN_EXE`, waits on a durable `TurnStart` marker (logged before that
  turn's GHC compile, so the kill lands inside a real compile window),
  `SIGKILL`s it by its own spawned PID, restarts, and asserts `generation`
  1 → 3 through `load_checkpoint(driver.checkpoint_path())`. No filename or
  JSON shape is hard-coded, which is why it survived F4 replacing the
  checkpoint format underneath it mid-wave. Waits are a progress-stall
  watchdog (no new durable event for 500s; 600s/800s absolute ceilings) rather
  than flat deadlines, after a flat deadline flaked under 4-6x contention.

- **Quality sweep** (`2e370d5c`) — workstream/finding-ID tags stripped (F17);
  `eval_in_binding` deleted (`pub`, zero callers, unleased, doc-link reference
  cleaned with it); `SelfHarnessState::Closing`/`is_idle`/`label` deleted;
  stale module docs corrected. Net −99 lines across 38 files.

## Test receipt

Batch commit tested: `06a822ea`.

```
--ignore-default-filter -j1 -p tidepool-harness -p tidepool-web \
  -E 'binary(turn_lease) | binary(selfharness_lifecycle) |
      binary(selfharness_persistence) | binary(selfharness_compaction_fixes) |
      binary(selfharness_spine) | binary(selfharness_compaction) |
      binary(crash_recovery)'
→ 16 tests run: 16 passed, 0 skipped [4392.1s]
```

quality-sweep's own seven-binary batch on its tip: 16 passed, 0 failed. Quick
tier on the final SHA: 119/119. `cargo check --workspace --all-targets` and
`cargo fmt --all -- --check` clean on `96f584dd`; clippy reports only three
pre-existing warnings (tidepool-codegen `large_enum_variant`, `engine.rs`
`TurnOutcome` `large_enum_variant`, `selfharness_compaction_fixes`
`type_complexity`) — unchanged, not this lane's.

**Caveat:** neither merge commit (the sweep fold, nor the final parent merge)
was itself run through the GHC battery. Both parents were; the sweep is
comment/dead-code only; post-merge verification was check/clippy/fmt/quick-tier.

**Transfer argument:** `persistence.rs`, `state_cross.rs`, `lifecycle.rs`,
`harness_source.rs`, `observer.rs` are unchanged against every tested SHA, so
the durability logic transfers. `harness.rs`/`engine.rs`/`driver.rs` changed in
the rebase, so the six binaries crossing that path re-ran.

## Merge resolution worth checking

The parent rewrote the `ghc-heavy` filter to default-deny — better and
self-maintaining, so its structure was taken. But its pure-Rust exemption list
names `package(tidepool-web)` wholesale, and `tidepool-web`'s `crash_recovery`
spawns the real selfharness binary (real extracts); taking it verbatim would
have uncapped it. That clause is now
`(package(tidepool-web) & !binary(crash_recovery))`. Verified: `cargo nextest
list -p tidepool-web` shows only `operator_gate`.

## Not done — the poison probe

Bounded probe, stopped at the quiesce boundary. Recipe:

```
git worktree add <dir> HEAD && cd <dir> && git revert --no-edit 9f2e18a5
TIDEPOOL_GC_POISON=1 TIDEPOOL_HEAP_VERIFY=1 TIDEPOOL_MAX_HEAP=16777216 \
cargo nextest run --ignore-default-filter -j1 -p tidepool-harness \
  -E 'binary(selfharness_compaction) &
      test(compaction_fires_mid_loop_in_place_and_reaches_next_render)' \
  --no-capture
```

`TIDEPOOL_MAX_HEAP` is the growth ceiling (default 1 GiB), so 16 MiB forces
constant collections; `GC_POISON` makes a stale read deterministic garbage
instead of load-dependent. Reproducing on the cb1b131d-restored tree and again
on the reverted tree exonerates the prune and yields a repro. The probe
worktree and branch were removed.

## Standing findings — reported, deliberately not acted on

- **`selfharness_compaction` is open-intermittent, not resolved.** Four
  observations: bare `d1e33cd0` PASS 193.3s; quality-sweep tip FAIL 65.7s;
  same tip PASS 201.2s; the reverted tip PASS 587.5s. The same tree both passed
  and failed, so the variable is not the tree — neither the branch nor
  `cb1b131d` is implicated by any of it.
- **`Harness::first_operator_hole` / `live_turn` / `pending_dialog_ui` /
  `tree_snapshot` / `tree_snapshot_page`** are `pub` with zero callers
  workspace-wide, apparently orphaned when the seven-pane observatory was
  replaced by the minimal gate. Not deleted: retiring an API cluster is an
  architectural call, not a sweep's.

## Corrections for the record

1. The regression escalation stated `PASS at 287e4068 / FAIL on d1e33cd0` as
   one evidence line. The failure was on a dev's branch tip, not the base; this
   tree never ran that binary on that SHA, because the binary had been trimmed
   from the batch. Two sources written up as one tree.
2. The symptom-class argument under it — "this area smells like constructor
   binding and that commit touched constructor binding" — was pattern-matching,
   not mechanism. The emission-path read (an unbound `VarId` hits a loud trap
   and cannot silently read a heap slot) is what settled it. A revert now sits
   on the tip that no evidence supports; re-land it on its merits, and a
   receipt claiming a reproduction would be false.
3. `selfharness_compaction` was trimmed from a batch by which *finding* each
   binary exercised rather than which *code paths* it compiles. The JIT sits
   under all of them equally, so that batch would have returned green with a
   live regression.

Boundaries honored: no four-state-machine consolidation; `effect_defs.rs`,
`preamble.rs`, the `finalize_shim` region, `service_runllm_hole`/
`AnswerContract`, and `compile.rs` timing untouched.
