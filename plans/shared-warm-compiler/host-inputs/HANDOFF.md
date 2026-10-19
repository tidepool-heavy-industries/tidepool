# Host-inputs execution handoff

Area branch `feature/shared-warm/host-inputs` lives in
`/srv/swarm/checkouts/tidepool-shared-warm-host-inputs-20261019`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. The imported peer implementation
is prerequisite source, not a campaign result. No host deployment is authorized.

## Ownership and live grants

| Native identity | Model / effort | Checkout / branch | State and outcome |
| --- | --- | --- | --- |
| `/root/warm_compiler_campaign/host_inputs` | Sol high | area checkout above | supervising issuer, admission and publication consumers |
| `/root/warm_compiler_campaign/host_inputs/selected_root` | `gpt-6.1-sol` / high, fresh | `tidepool-shared-warm-selected-root-20261019`, `feature/shared-warm/selected-root` | running; approved exact-context input reuse implementation at `08feadc4` |
| `/root/warm_compiler_campaign/host_inputs/selection_oracle` | `gpt-6.1-sol` / high, fresh | read-only area source | running; source-support adoption bridge and physical-refusal review |

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
- Selected-query successor `9dbc0e18..c31f92ee` is merged in area
  `08feadc4ea5de330b6ab00b10399b8a65d4bb7ae`. Prepared value support no longer
  captures a full selected metadata map: exact Val verification borrows its
  admitted interface, and lexical membership uses a distinct-owner set. The
  actual source-support owner still carries Original roles and exact edges.
  Independent exact and cumulative source review found no code defect:
  `selected-query-review-c31f.txt` and `cumulative-review-08fe.txt` in the oracle
  evidence directory above. Compilation/test execution remains 0/0.
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
| Contextual sealing | `PreparedCellProgram::seal` -> `PreparedValueSourceSupport::new` -> each `retain_output` | lazy first-value preparation and selected metadata deletion implemented/source-reviewed; lexical/import/implementation folds remain for consuming cells |
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
swarm-build just test-lib tidepool-toolchain --exact declaration_context::tests::prepared_value_source_support_borrows_selected_rows_and_refuses_dormant_owners --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::tests::retained_value_source_surface_preserves_selected_support_and_refuses_hidden_owners --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts --expected-count 1
swarm-build just test-lib tidepool-runtime --exact session::turn::tests::prepared_cell_seals_after_parent_runtime_drops_and_projection_end_settles --expected-count 1
swarm-build just test-lib tidepool-runtime --exact session::turn::tests::cell_program_host_cancellation_after_end_refuses_sealing_without_new_request --expected-count 1
```

Discover exact affected production/test labels through the admitted configured
entry point before compiling; linking alone is not execution. No artifact or
executed count exists for campaign changes yet.

## Next actions

The two leaves now own a concrete root/transition constructor and consumer
frontier. Reuse the existing `Arc<DeclarationContextOwner>` for
the complete baseline context: support identity alone omits producer, lexical,
template and original-instance facts. Effective private/protected selection
remains with `RequestCompilerInputs.source_support`; runtime paired value/alias
visibility and current admission remain separate. Do not add a second identity
registry or wrap support just to mint an owner.

The campaign approved a minimal real consumer first:
`ProgramAdmissionContinuation::in_program_context` supplies no new private input
and inherits the exact private/scaffold owner. A context-only
`ExactRequestInputs` method may return its existing `Arc` on exact complete
context-owner identity after producer/cancellation checks. Private arrivals and
all changed owners still take checked admission. This bypasses repeated compiler
input construction only; it does not bypass operation-local authority or demand.
Implementation is running in `selected_root`; exact candidate/review is pending.

Independent review caught an unsafe broader shortcut before acceptance:
`with_inputs` freshly validates actual package-interface paths/digests through
`PackageInterfaceValidation` on each continuation. Same immutable context owner
does not own those mutable bytes. Remove/mutate an external `.hi` between two
same-owner continuations and the second must still refuse. Owned-inputs confirms
no immutable captured/pinned package lease is approved. The first reuse parcel
therefore retains existing group/package validation unchanged; no group-path
skip or new cross-operation package memo is authorized.

Actual ordered cursor lag must be repaired at its issuing boundary:
`admit_program_support` prepares `continued.inputs` before it publishes source
support, then `admit_program_value` extends that newer context while storing the
older request. A value-only proof cannot certify the whole next input change.
Certify/adopt source support first, then compose an operation-local value
transition holding exact old/new owners. Persistent roots must not hold a strong
predecessor chain. Even value-only additions can change `HomePresence`, so native
group equality does not by itself permit reusing Package absence conclusions.
The custody-owner collision query in `prepare_value_interface_append` still
scans all acquired payloads; a persistent acquisition membership index belongs
to `ViewLease`, distinct from the selected-home census. Its fields are reserved
with owned-inputs; no implementation started yet.

Physical collaboration: rejected an unconditional materialization-frontier
refresh after each value because it would add a full closure/metadata scan.
Rows owner instead promotes the exact produced leaf before the existing
`retain_output` / `source_support` physical projection, and host bindings before
`select_value_interface`. No compiler-projection helper is needed. Physical
candidate `3716265d` is owned and reviewed by owned-inputs, not folded here.
Native-offer/prototype retention remains separately observable; no required
owner may be erased merely to make a lifetime assertion pass.

An ordinary `exact.value_source_support.prepare` constructor trace span supports
future zero-construction declaration-only / first-value mixed-cell checks; those
are not executed yet. On concrete root/transition proposal arrival the area
arbitrates with campaign/warm/physical owners, assigns the minimal accepted
production bridge, obtains independent exact review, repairs and folds full
history. No periodic polling or wake loop: when dependency-bound, retain this
waiting state and yield. The result
producer checks recipient state and uses `followup_task` for a completed/idle
non-root parent, with exact candidate, evidence and next action; `send_message`
suffices only while the recipient is running.

Broad 02/03 P1/P4/publication migration still needs the compiled issuing contract,
complete negative dependency controls and joined production evidence. Independent
source design/constructor implementation for 01 remains authorized despite the
native disk gate. Preserve remaining warm folds and resource blockers explicitly;
source review does not make this area integration-ready or qualified.
