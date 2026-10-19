# Host-inputs execution handoff

Area branch `feature/shared-warm/host-inputs` lives in
`/srv/swarm/checkouts/tidepool-shared-warm-host-inputs-20261019`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. The imported peer implementation
is prerequisite source, not a campaign result. No host deployment is authorized.

## Ownership and live grants

| Native identity | Model / effort | Checkout / branch | State and outcome |
| --- | --- | --- | --- |
| `/root/warm_compiler_campaign/host_inputs` | Sol high | area checkout above | supervising issuer, admission and publication consumers |
| `/root/warm_compiler_campaign/host_inputs/selected_root` | `gpt-6.1-sol` / high, fresh | `tidepool-shared-warm-selected-root-20261019`, `feature/shared-warm/selected-root` | running; bounded selected-interface lookup successor, base `9dbc0e18a2e05dfa1e7726af27fd598bc4886f2c` |
| `/root/warm_compiler_campaign/host_inputs/selection_oracle` | `gpt-6.1-sol` / high, fresh | read-only area source | settled after cumulative review; reserved for exact successor review |

Two descendant grants retained; no deeper descendants permitted. Starts,
reviewer settlement/reactivation and repaired candidate review were reported to
the campaign owner. No grant was transferred outside this area.

`selected_root` owns `compiler_projection.rs` selected-support representation
and tests. The owned-inputs area owns physical checked-value row borrowing in
`checked_cell.rs` and bounded row materialization in `declaration_context.rs` /
`owned_input_arena.rs`. Host logical P1/P4/P6 changes require coordinated symbols.
Reload owns current generation permission; borrowing an exact input root cannot
grant runtime authority. Warm-worker portable root/delta exchange awaits a
reviewed concrete issuance contract.

## Current decisions and findings

- Extend `CompilerSourceSupport`, which already retains selected exact scoped
  bindings, acquisitions, roles, availability and decisions; no parallel registry.
- Initial bounded implementation changes `AdmittedCompilerRoles.rows` from a
  `BTreeMap` copied on fresh value append to a persistent map. This alone does
  not establish incremental admission or publication. Full history
  `5b74c2f1..14d092fd` is merged in area checkpoint
  `9dbc0e18a2e05dfa1e7726af27fd598bc4886f2c`, along with lazy P6 commit
  `85e5f3ced0`. Independent review caught and repaired a test retaining a fork
  handle during parent removal. Source review accepts the cumulative change;
  compilation and execution remain pending. Receipts:
  `/srv/swarm/evidence/inanna/shared-warm-host-inputs-20261019/oracle/cumulative-review-9dbc.txt`
  and `lazy-seal-and-repair-review.txt` in the same directory.
- Accepted C2 amendment: campaign `e2bd2611102566085d0c573568d820a50ed62fb3`,
  inspected directly. The selected row's exact binding and admitted payload is
  its home witness: canonical finalization, checked Val certificate or accepted
  lexical join. Preserve the separate type-name `home_units` query. Base-bound
  transitions retain exact old/new selected owners, not payload equality.
- `OriginalValidationInventory::new` currently builds its module census from
  every supplied available original before selected-group traversal. It includes
  zero-group originals and has no interface-only input. It is not yet a proven
  selected-world census. The proposed successor census consists of exactly
  selected declared home owners, including authenticated interface-only owners;
  dormant custody and executable demand do not issue membership. Production
  callers pass retained maps including custody roots, while fresh native-offer
  validation has a deliberately scoped producer census. Do not narrow exact
  carrier authentication or `native_ids` wholesale when migrating this query.
  Independent issuer/caller tracing and minimized insertion/removal/fork recipes:
  `/srv/swarm/evidence/inanna/shared-warm-host-inputs-20261019/oracle/selected-world-census.txt`.
- The campaign approved independent lazy P6 support preparation. Declaration
  preparation already admits each source and continuation through the exact
  context's `extend` / `finish` and `in_program_context_with_private_input`.
  `PreparedCellProgram::seal` now builds contextual value support only before
  the first native value item consumes it. Declaration prefixes are local
  immutable assembly; cancellation checks remain per item and before candidate
  publication. The contextual constructor can still refuse before native target
  capture; no declaration-only value support is built.
- Keep optional equal-interface body replacement and strict conflict refusal
  for genuinely required historical edges. Exact scoped child edges and private
  root permissions remain immutable facts, separate from direct seeds.
- The stronger shared-workspace Persona baseline report passed 1/1 with exit 0;
  compiler cleanup was `not_observed` and diagnostics incomplete. It is baseline
  evidence, not campaign qualification.

## Production mutation and consumer map

This is an in-progress source-backed inventory; unlisted paths remain unqualified.

| Operation | Current issuer / next consumer | Repeated work requiring migration |
| --- | --- | --- |
| Fresh value-interface append | `CompilerSourceSupport::admit_value_interfaces` / selected interface queries | persistent selected rows implemented and source-reviewed; actual counted clone bound and native controls pending |
| Retained value input capture | `artifacts::checked_value_context` -> `ExactDeclarationContext::extend_retained_value_artifacts` -> `prepare_compilation` | per-value support merges, lexical union, producer and final normalization |
| Ordered support admission | `ProgramSourceSupportState::admit_program_support_inner` -> next item request | selected metadata maps, inventory descriptors, lexical graph reconstruction |
| Contextual sealing | `PreparedCellProgram::seal` -> `PreparedValueSourceSupport::new` -> each `retain_output` | lazy first-value preparation implemented; lexical/import/implementation and selected metadata folds remain for consuming cells; selected metadata deletion successor running |
| Value publication / capture | `CheckedValueArtifact::capture_output` retains support and templates / next checked-value context | must carry exact completed per-item support; physical row ownership belongs to owned-inputs |
| Declaration public restaging | `capture_declaration_target` -> `extend_admitted_surface` -> join certification | lexical expansion and root `sort` / `dedup`; stale validation and reservation semantics remain mandatory |
| Resident declaration commit | `prepare_resident_declaration` -> `commit_prepared_declaration` | coherent existing scope tip already carries `ScopedCompilerContext`; extend this owner rather than actor-current registry |
| Prepared declaration successor | `PersistentSession::prepare_declaration_successor` -> successor visibility/view | binding and declaration enumeration, view recanonicalization and admission hashing |
| Compile view capture | `scoped_compile_view_in` -> `compile_view_in` / runtime admission | cached view projection but live injected inventory enumerated per request; visible/reachable/name maps rebuilt on relevant changes |
| Cold recovery | `hydrate_context_roots` -> acquired exact contexts -> current scoped context | cold authenticated acquisition deliberately allowed; do not restore role authority from bytes alone |

## Resources and validation

Area and implementation checkouts have independently provisioned bind mounts.
Observed `/srv/build`: 102 GiB available, with 100 GiB required unreserved.
`swarm-build --storage-status` exits 2 with `exec: --: invalid option` on the
installed launcher. No cold 32 GiB build has been admitted; no cache deletion,
admission bypass or shared daemon restart was attempted.

Production and test compilation, execution, and performance are **unqualified**.
Exact pending mandatory controls (run in the implementation checkout through
the pinned admitted workflow when disk admission is resolved):

```sh
swarm-build bash scripts/buck2-configure.sh --tests
swarm-build bash scripts/buck2-run.sh targets -c remote.enabled=false //tidepool/toolchain:
swarm-build just build //tidepool/toolchain:tidepool_toolchain //tidepool/toolchain:tidepool_toolchain_unit_tests_binary
swarm-build just test-lib tidepool-toolchain --exact artifact_inventory::compiler_projection::tests::value_publication_forks_share_unchanged_selected_rows --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact artifact_inventory::compiler_projection::tests::fresh_value_append_preserves_native_permissions_and_exact_execution_roots --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts --expected-count 1
swarm-build just test-lib tidepool-runtime --exact session::turn::tests::prepared_cell_seals_after_parent_runtime_drops_and_projection_end_settles --expected-count 1
swarm-build just test-lib tidepool-runtime --exact session::turn::tests::cell_program_host_cancellation_after_end_refuses_sealing_without_new_request --expected-count 1
```

Discover exact affected production/test labels through the admitted configured
entry point before compiling; linking alone is not execution. No artifact or
executed count exists for campaign changes yet.

## Next actions

Waiting for `selected_root` to deliver its exact base `9dbc0e18..candidate`
successor: remove the full stored selected metadata map in
`PreparedValueSourceSupport` and query its existing exact admitted rows at
`retain_output` / `retain_surface`. Reserved methods are the struct and `new`,
`retain_output`, `retain_surface`, plus instrumentation only in
`compiler_metadata_snapshot`. Physical materialization symbols remain with the
owned-inputs area. Test/counter controls must preserve bad Val/unowned output
refusal, distinct lexical membership, source Original selection and custody.
An ordinary constructor trace span will support joined zero-construction
declaration-only / first-value mixed-cell checks; those are not executed yet.

On arrival, area supervisor reviews directly, reactivates `selection_oracle`
for exact independent review, routes bounded repairs and folds the full history.
Send source-reviewed checkpoints to the campaign owner. No periodic polling or
wake loop: when dependency-bound, retain this waiting state and yield. The result
producer checks recipient state and uses `followup_task` for a completed/idle
non-root parent, with exact candidate, evidence and next action; `send_message`
suffices only while the recipient is running.

Broad 01 selected transitions and 02/03 P1/P4/publication migration still need
the compiled issuing contract, complete negative dependency controls and joined
production evidence. Preserve remaining warm folds and resource blockers
explicitly; source review does not make this area integration-ready or qualified.
