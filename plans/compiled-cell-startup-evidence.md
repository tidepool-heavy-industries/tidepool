# Compiled-cell startup and recovery evidence

Recorded 2026-10-01 UTC. This is a focused startup checkpoint, not acceptance
of the whole delivery plan.

## Frozen eight-case run

Source: `225c59896187004b274f2e4ad5459f97c5e94c11`, including joined root
`4422521a66fa7183c8af475388b04d630f430a36`, actor journal version 5,
deferred executable startup entries, and the inline typed-site compiler repair.
The source and executable stayed frozen throughout execution.

The default facade library test target compiled in 75 seconds. Nextest run
`4d489d60-cf5d-4843-82be-b9f2e8e09cd6` executed **8 cases: 7 passed, 1 failed,
624 skipped**, in 2711.386 seconds, exit status 100. The real Haskell worker
and Rust compiler frontend were prepared through the repository Nix shell;
this was an incremental build and compiler-backed execution, not a cold build
or a synthetic startup-only simulation.

All names below have prefix `actor_host::embedded_recovery_tests::`.

| Exact test | Result and boundary |
|---|---|
| `production_authored_root_failure_is_not_evaluated_before_durable_binding` | Passed. A real authored pure error was installed, then the child exited at durable ApplicationBound without evaluating it, attaching a provider, or reporting readiness. |
| `production_missing_manifest_refuses_bound_root_without_rewriting_journal` | Passed. Bound-root manifest loss refused without recreating the manifest or changing the retained journal. |
| `production_old_startup_journal_is_refused_without_rewriting_evidence` | Passed. Raw journal versions 3 and 4 refused without changing their bytes. |
| `production_repeated_startup_crashes_roll_split_owners_forward_without_replay` | Passed. Nine separate-process interruptions at admission, manifest, Store, Bound, and release; independent manifest/Store owners verified; final readiness produced no replay and exactly one call after fresh input. |
| `production_startup_and_cold_successor_preserve_bound_conversation_without_replay` | Passed. A killed host's successor retained the exact durable conversation head, transferred binding, made no replay call, and handled one newly authenticated input. |
| `production_uncertain_store_write_never_activates_visible_binding` | Passed. A real SQLite reader allowed the binding commit to become visible while FULL WAL durability confirmation failed. No activation followed that error; a fresh successor recovered without replay. |
| `startup_head_follows_predecessors_across_fresh_ids_and_lower_incarnations` | Passed. Unit evidence for linear predecessor selection across fresh IDs and lower incarnation numbers, stopping at the first Bound record and refusing disconnected heads. |
| `production_cold_successor_executes_retained_original_declaration_in_fresh_heap` | Failed after 345.058 seconds during the first host's authored declaration publication: `artifact inventory: incomplete interface requirements`. The successor was not started. This run does **not** prove original declaration execution in a fresh heap. |

The exact selected command was:

```sh
systemd-run --user --scope --slice=tidepool-completion-build.slice \
  --property=MemoryHigh=16G --property=MemoryMax=20G --property=MemorySwapMax=1G \
  bash scripts/dev-shell.sh env CARGO_BUILD_JOBS=4 \
  CARGO_TARGET_DIR=/tmp/tidepool-compiled-cell-startup-facade/target \
  TIDEPOOL_KEEP_TEST_LOGS=1 TIDEPOOL_DAEMON_ARGS='--workers 1 --rss-ceiling-mb 10240' \
  bash scripts/battery.sh -p tidepool --lib --locked --offline --test-threads 1 \
  -E 'test(=actor_host::embedded_recovery_tests::startup_head_follows_predecessors_across_fresh_ids_and_lower_incarnations) | test(=actor_host::embedded_recovery_tests::production_old_startup_journal_is_refused_without_rewriting_evidence) | test(=actor_host::embedded_recovery_tests::production_missing_manifest_refuses_bound_root_without_rewriting_journal) | test(=actor_host::embedded_recovery_tests::production_uncertain_store_write_never_activates_visible_binding) | test(=actor_host::embedded_recovery_tests::production_repeated_startup_crashes_roll_split_owners_forward_without_replay) | test(=actor_host::embedded_recovery_tests::production_startup_and_cold_successor_preserve_bound_conversation_without_replay) | test(=actor_host::embedded_recovery_tests::production_cold_successor_executes_retained_original_declaration_in_fresh_heap) | test(=actor_host::embedded_recovery_tests::production_authored_root_failure_is_not_evaluated_before_durable_binding)'
```

Retained host evidence:

- `/tmp/startup-facade-v5-final-tests.log`: build output and exact nextest summary.
- `/tmp/tidepool-compiled-cell-startup-facade/target/tidepool-test-runs/20261001T022644Z-3685067-battery/`: compiler JSONL and `reproduce.sh`.
- `/tmp/tidepool-compiled-cell-startup-facade/target/recovery-fixtures/production-recovery-h8Uaa7/publish-original.log`: failed real declaration publication.
- `/tmp/tidepool-compiled-cell-startup-facade/bridge/facade/target/tidepool-test-runs/compiler-failures/.tmpjaAoy0`: first failure's retained compiler artifacts.

The owned daemon PID 3685190 and final worker PID 3774624 were torn down;
their absence was checked after the run. No live provider or Codex run was used.

## Subsequent fixture correction

`02ebd8b7e` corrects the cold-original fixture to authenticate its fresh browser
session rather than reusing a cookie from the killed host. This is a test-only
change after the frozen run; it does not turn the failed case into passing
evidence. Its default facade library test target compiled successfully in
24.57 seconds with `cargo test -p tidepool --lib --no-run --locked --offline`
inside the same admitted Nix environment. Log:
`/tmp/startup-facade-cookie-compile.log`. Rust formatting and `git diff --check`
passed. The compiler dependency repair and exact single cold-original execution
remain required; the seven completed startup cases need not be repeated solely
to diagnose that publication failure.
