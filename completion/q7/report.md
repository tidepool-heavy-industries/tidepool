# Q7/Q8 completion report — publication alternatives, terminal drainage, route deletion

- Checkout: `/srv/swarm/checkouts/tidepool-completion-q7-lifecycle-20261019`
- Branch: `completion/q7-lifecycle` (not pushed)
- Base: `099c91d97f57de9a47e735137cd8ac7870027c41`
- Commit 1 (semantic repairs): `309b4c6c62` — Preserve publication authority and compound renderer failures
- Commit 2 (old-route deletion): `49decd51c4` — Delete the prepared split-install route
- Working tree after commit 2: clean except ignored `completion/` evidence

## What each commit contains

### `309b4c6c62` — semantic repairs (kept separate as directed)

- `ActorPublicationGuard`: the three `Option` fields (`world`, `target_fence`,
  `target_prerequisite`) are replaced by one private
  `PublicationAuthority::{World, TargetFence}`. Settlement consumes the
  authority (`refused_when` / `confirmed_when`); Drop keeps the conservative
  unresolved outcome. Concurrent-claim check and exact attempt identity retained.
- `InstalledToolsState(Mutex<InstalledToolsRecord>)` owns both slots
  (`availability`, `reply`) as one record. `Retired` is distinct from fresh
  `Empty`; `drain_retired() -> InstalledToolsDrain` takes installation and
  reply custody under one terminal transition and drops the extracted owners
  after releasing the lock. A later publisher cannot reinstall into `Retired`.
  An issued request snapshot is retained independently of the drain.
- `RendererProductionFailure` primary (action) plus secondary
  (close/publication) evidence crosses the actor error/receipt boundary;
  `action_error.unwrap_or_else(...)` collapse removed. Recovery and presentation
  consume both facts.
- Evidence: `completion/q7/commit1-compile.log` (BUILD SUCCEEDED),
  `completion/q7/commit1-focused-tests.log` (10 passed; 0 failed; 3 ignored,
  exit 0). Earlier full-WIP evidence remains in `completion/q7/compile*.log`
  and `completion/q7/focused-*.log`.
- Known pre-existing fixture failure retained in
  `completion/q7/explicit-replacement-control.log`
  (`SourceAdmission(Unqualified)` in the explicit-replacement fixture); not a
  regression of this parcel and not repaired here.

### `49decd51c4` — delete the prepared split-install route

Deleted outright, with callers:

- `PendingPreparedMode`, `PendingPreparedInstall` (including
  `PendingPreparedInstall::compile_off_checkout` and its fresh cancellation
  token / `.action`-only extraction), `PendingPreparedSource` (both `Legacy`
  and `Certified`), `ReadyPreparedSource`, `ReadyPreparedInstall`,
  `PreparedInstallMetadata`.
- `ResidentSession::snapshot_run_prepared`,
  `ResidentSession::revalidate_and_run_prepared`,
  `ResidentSession::prepared_machine_ready` (equivalent `is_bootstrapped()`
  retained), and the `tidepool::session` re-exports.
- `ValueInterfaceSource`, `ValueInterfaceSource::LegacyDisk`, `Checked`,
  `Staged`, and `for_checked`. `bind_prepared` now takes
  `Option<StagedCheckedValueInterface>`; `None` means the install retains no
  in-memory value interface (previously the `LegacyDisk` tombstone). Unit
  selection is derived from `checked.is_some() || staged_interface.is_some()`.
  `mark_legacy_value_interface` is no longer called from `resident.rs`.
- Actor split install: `begin_ready_block_split`, `pending_mode_for`,
  `cloned_turn_code`, `PreparedSnapshotAttempt`, `CHEAP_RETRY_ATTEMPTS`,
  `log_stale_native_imports`. `bind_item` now enters
  `begin_ready_bind_in_checkout` inside one `with_compiler_machine` checkout,
  the same install entry used by `begin_prepared_cell_item`.
- Tests/fixtures that only enter the deleted route (see table below):
  six `compile_fail`/`compile_pass` `prepared_install_*` fixtures and their
  `BUCK` registrations, the `native-workflow.py` compile-fail rows,
  `prepared_residency.rs::certified_resident_turn_compiles_off_checkout_then_installs`,
  the two `call_timing_tests` off-checkout telemetry tests, the snapshot
  refusal block in `activation_input_tests.rs`, and five inline
  `authored_publication_tests` plus the `integer_capsule_session` helper in
  `resident.rs`.

Retained deliberately:

- `tidepool/runtime/tests/prepared_unit_codegen_cost.rs` (including
  `a_later_unit_reuses_an_earlier_unit_s_generated_code`) — B-04 Cranelift
  reuse carrier, untouched.
- Every other `prepared_residency.rs` assertion: only the one split-install
  test was removed.
- `run_turn` and the startup fallback (A-11): still reachable and not deleted
  in this pass. Feynman takes A-01/A-02; A-11 waits for the service startup
  path.
- Finite admission call sites at `resident_workbench.rs:8793` /
  `:8805` (base `8513`/`8525`): unchanged pending R1's caller patch (below).

## Commands and results

All builds through `swarm-build` with the checkout's pinned action PATH
(`export PATH="$(awk -F ' = ' '$1 == "action_path" {print $2}' .buckconfig.local):/run/current-system/sw/bin"`).

| Purpose | Command (targets) | Result | Evidence |
|---|---|---|---|
| Commit 2 compile | `buck2 build --local-only -c remote.enabled=false -c tidepool.profile=fast-dev //exomonad/actor:exomonad_actor_unit_tests //tidepool/runtime:tidepool_runtime` | BUILD SUCCEEDED; 10 commands, 0 cached/remote | `completion/q7/commit2-compile.log` |
| Host test compile (deleted telemetry tests) | same flags `//exomonad/host:exomonad_host_unit_tests` | BUILD SUCCEEDED; 437 commands | `completion/q7/commit2-compile-host.log` |
| Focused surviving bind control | `just test-lib exomonad-actor --exact resident_workbench::request_tests::joined_source_hook_refusal_precedes_binding_and_native_exports --expected-count 1` | 1 passed; 0 failed; 3 ignored; exit 0 | `completion/q7/commit2-focused-test.log` |
| Runtime unit-test target | same flags `//tidepool/runtime:tidepool_runtime_unit_tests` | BUILD FAILED, exit 3, single E0599 at `turn.rs:9623` (`prepared_machine_ready`); library target above is green | `completion/q7/commit2-runtime-tests-blocked.log` |

`git diff --check` is clean. Pinned `rustfmt --check` reports only
pre-existing diffs in files touched by neither commit
(`activation_input_tests.rs:407`, `compiler_query.rs:35`); no diff in the
edited regions.

## R1 routing (files not owned here, not edited)

1. `tidepool/runtime/src/session/turn.rs:9623` still calls the removed
   `resident.prepared_machine_ready()`. Required one-line edit:
   `!resident.prepared_machine_ready()` → `!resident.is_bootstrapped()`.
   `is_bootstrapped()` is the retained public wrapper
   (`resident.rs:6465`, delegating to `persistent.rs:1230`) and is exactly
   what `prepared_machine_ready` returned. This is the only blocker for
   `//tidepool/runtime:tidepool_runtime_unit_tests`.
2. `tidepool/runtime/src/session/admission.rs` retains
   `RetainedValueInterface::LegacyDisk` producers:
   `admission.rs:4357,4376` (tests) and `persistent.rs:4708` (test). The
   production producer (`ValueInterfaceSource::LegacyDisk`) is gone. Deleting
   the residual marker type and `mark_legacy_value_interface` requires edits in
   R1-owned `admission.rs` tests; left for R1.
3. Caller patch deliverable (not applied): the finite admission call sites at
   `exomonad/actor/src/resident_workbench.rs:8793` (`admit_host_carrier_cell_in`)
   and `:8805` (`admit_planned_cell_for_execution`) remain on the finite basis.
   Apply the `completion/r1/CALLER-PATCH.md` shape only after R1's
   `admit_world_planned_cell_for_execution` wiring lands, and confirm the exact
   signature with Feynman before editing. No such patch file exists in this
   checkout/evidence tree yet.
4. Out of scope, same owner family: `PreparedEngine::compile_off_checkout` and
   `InstallSnapshot` in `tidepool/runtime/src/session/prepared.rs` now have
   only `#[cfg(test)]` callers. The engine-level snapshot compile and its
   `compiled_off_checkout` telemetry can be deleted by the `prepared.rs` owner;
   they are not a second supported route in production.

## Stale-surface ledger disposition

- A-09: **fixed in this commit.** The split install (`begin_ready_block_split`
  and its bootstrap fallback/retry) is deleted. Certified and uncertified
  prepared turns now both install through `begin_ready_bind_in_checkout` under
  one machine checkout; there is no separate "uncertified" install path.
- A-10: **fixed for the production producer.** `ValueInterfaceSource::LegacyDisk`
  and `for_checked(false)` are deleted; a missing checked interface is now an
  absent interface rather than a disk-fallback tombstone. Residual test-only
  marker producers are listed under R1 routing (2).
- A-11: **retained and why.** `run_turn` remains reachable as the startup
  fallback (`exomonad/host/src/actor_host.rs:3324`). Deleting it now would
  remove the only startup path before the service startup issuer exists;
  Feynman owns it after A-01/A-02.

## LOST / covered table

`LOST:` means no surviving assertion in this branch covers the guarantee; it
does not mean the behaviour is wrong, only that the evidence is gone.
`VACUOUS` means the represented state no longer exists.

| Deleted control | Guarantee it carried | Status | Surviving coverage / owner |
|---|---|---|---|
| `prepared_residency.rs::certified_resident_turn_compiles_off_checkout_then_installs` | A certified resident turn compiles off-checkout and then installs under a revalidated checkout | **LOST** | No surviving route compiles a turn off-checkout. `prepared_unit_codegen_cost.rs` retains engine-level Cranelift reuse (B-04); replacement route contracts are owned by the B-03/B-04/B-05 leaf |
| `call_timing_tests::second_cell_install_compiles_off_checkout` | A second cell uses the split off-checkout install, observable by `compiled_off_checkout=true` | **LOST** | Telemetry-only oracle; the route is deleted and no surviving install reports off-checkout |
| `call_timing_tests::six_unit_cell_compiles_every_unit_off_checkout` | Every unit of a multi-unit cell compiles/installs off-checkout | **LOST** | Same; tests plan item B-01 replacement owned by the B-03/B-04/B-05 leaf |
| `compile_fail/prepared_install_consumes_pending` (+`.stderr`, `compile_pass` control) | `PendingPreparedInstall` is affine: compiling consumes it exactly once | **VACUOUS** | Type deleted; nothing affine remains on the install route |
| `compile_fail/prepared_install_crossed_image` (+`.stderr`, `compile_pass` control) | A pending install cannot cross to another machine image | **VACUOUS** | Type deleted; single-checkout install has no cross-image payload |
| `resident.rs::source_fence_keeps_precompiled_plain_execution_and_refuses_ready_install` | A closed source fence refuses a ready split install with `SourceAdmission::Closed` and no code/table/binding/parked mutation, while an already-prepared plain startup still runs | **PARTIAL** | Closed-owner refusal is covered at the owning point by `prepared_target_tests::prepared_targets_reject_foreign_settlement_and_retired_original` and `turn.rs` restage controls; route-level "ready install" refusal is **LOST** |
| `resident.rs::ready_install_keeps_original_source_when_recipient_owner_changes` | Split install keeps the original source A when the recipient owner changes | **LOST** (vacuous for installs) | No cross-owner install route remains. Original-vs-target authority is covered for the surviving activation-preview route by `activation_input_tests` original/target controls |
| `resident.rs::prepared_capsules_keep_same_shape_images_and_original_binding_metadata_together` | Capsule fields (same-shape images, demanded images, original binding metadata) stay coupled | **VACUOUS** | Capsule type deleted. Replacement image/metadata contracts owned by the B-03/B-04/B-05 leaf |
| `resident.rs::dropping_uninstalled_prepared_capsules_does_not_execute_or_retain_metadata` | Dropping an uninstalled capsule neither executes nor retains metadata | **VACUOUS** | Capsule type deleted; no deferred install object remains |
| `resident.rs::certified_install_fences_hidden_binding_replacement` | A ready capsule refuses to install after a hidden binding was replaced (revalidation returns `None`, residency unchanged) | **LOST** | Cross-checkout revalidation no longer exists; single-checkout install resolves against live bindings under the same checkout, so the stale-capsule state is not reachable. No route-level replacement assertion survives |
| `activation_input_tests` snapshot-refusal block | `snapshot_run_prepared` refuses preview code with `Unqualified` before spending the affine preview admission | **PARTIAL** | The same test still asserts `admit_turn_code_in` and `run_with_sites` refuse with `Unqualified` and leave `admission().is_consumed() == false`; only the snapshot entry is gone |
| `turn.rs` edited-binder assertion (unowned) | Edited binder metadata never reaches native install | **PENDING R1** | Coverage is the assertion itself; apply the one-line `is_bootstrapped()` edit listed above |

## Files in commit 2

`exomonad/actor/src/resident_workbench.rs`, `exomonad/host/src/actor_host/call_timing_tests.rs`,
`scripts/native-workflow.py`, `tidepool/runtime/BUCK`,
`tidepool/runtime/src/session/{activation_input_tests,mod,resident}.rs`,
`tidepool/runtime/tests/prepared_residency.rs`, and the six deleted
`tidepool/runtime/tests/{compile_fail,compile_pass}/prepared_install_*` files.

## Follow-up parcel: deleted bounded-browse consumer (commit 3)

Commit `cb0fcfbc8a` — "Drop the actor filter for the deleted bounded browse".

Aristotle removed `InspectionQuery::BrowseBounded` and `ViewIdentity` /
`view_identity()` from `tidepool/runtime/src/session/inspection.rs`
(`completion/inspect-delete`, `d99ab38fc7`). The actor's
`compiler_query.rs` no-enumerating-verb test was the last consumer.

- Removed only the stale `compiler_verbs()` filter arm matching
  `InspectionQuery::BrowseBounded`; no replacement filter was added.
- Corrected the `QUERY_RESULT_LIMIT` doc comment (`BrowseBounded` →
  only `TypeSearchBounded`).
- `rg` over `exomonad/actor/src/` finds no `BrowseBounded`, `ViewIdentity`,
  or `view_identity` references.
- `inspection.rs` was not edited or committed here; the branch builds at its
  base without the deletion.

Verification (pinned action PATH through `swarm-build`):

| Purpose | Command | Result | Evidence |
|---|---|---|---|
| Actor unit-test target | `buck2 build --local-only -c remote.enabled=false -c tidepool.profile=fast-dev //exomonad/actor:exomonad_actor_unit_tests` | BUILD SUCCEEDED; 8 commands, 0 cached/remote | `completion/q7/inspect-delete-compile-base.log` |
| Same, with `d99ab38fc7` temporarily applied, then reverted | same | BUILD SUCCEEDED | `completion/q7/inspect-delete-compile.log` |

Remaining unproduced wire support for bounded browse (reported, not edited;
separate parcel):

- `tidepool/extract-cmd/src/request.rs:503` variant; `:722` inspection-mode
  predicate; `:877` CLI flags; `:880-884` field construction; `:987` wire
  decode (tag 62); `:1414` `inspect_browse_bounded`; `:1549` flag encoding;
  `:1957` wire encode (tag 62); `:2307`, `:2313` round-trip test.
- `tidepool/extract-cmd/src/lib.rs:847-849` public builder.
- `bridge/haskell/src/Tidepool/ExtractRequest.hs:231` exported constructor;
  `:488` `InspectModuleBounded`; `:578-579` CLI mapping; `:746` encode;
  `:895` decode.
- `bridge/haskell/src/Tidepool/Introspection.hs:607` execution case.
