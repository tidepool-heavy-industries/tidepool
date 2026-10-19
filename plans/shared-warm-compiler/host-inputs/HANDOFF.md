# Host-inputs execution handoff

Area branch `feature/shared-warm/host-inputs` lives in
`/srv/swarm/checkouts/tidepool-shared-warm-host-inputs-20261019`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. The imported peer implementation
is prerequisite source, not a campaign result. No host deployment is authorized.

## Ownership and live grants

| Native identity | Model / effort | Checkout / branch | State and outcome |
| --- | --- | --- | --- |
| `/root/warm_compiler_campaign/host_inputs` | Sol high | area checkout above | supervising issuer, admission and publication consumers |
| `/root/warm_compiler_campaign/host_inputs/selected_root` | `gpt-6.1-sol` / high, fresh | `tidepool-shared-warm-selected-root-20261019`, `feature/shared-warm/selected-root` | running; initial persistent exact selected-row tranche |
| `/root/warm_compiler_campaign/host_inputs/selection_oracle` | `gpt-6.1-sol` / high, fresh | read-only area source | running; independent selected-world census and optional/required-root semantics, then exact-candidate review |

Two active descendants granted; no deeper descendants permitted. Starts were
reported to the campaign owner. No settlements or reallocations yet.

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
  not establish incremental admission or publication.
- `OriginalValidationInventory::new` currently builds its module census from
  every supplied available original before selected-group traversal. It includes
  zero-group originals and has no interface-only input. It is not yet a proven
  selected-world census. The proposed successor census consists of exactly
  selected declared home owners, including authenticated interface-only owners;
  dormant custody and executable demand do not issue membership. Campaign
  arbitration and independent issuer tracing remain pending.
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
| Fresh value-interface append | `CompilerSourceSupport::admit_value_interfaces` / selected interface queries | selected row map clone; first leaf owns repair |
| Retained value input capture | `artifacts::checked_value_context` -> `ExactDeclarationContext::extend_retained_value_artifacts` -> `prepare_compilation` | per-value support merges, lexical union, producer and final normalization |
| Ordered support admission | `ProgramSourceSupportState::admit_program_support_inner` -> next item request | selected metadata maps, inventory descriptors, lexical graph reconstruction |
| Contextual sealing | `PreparedCellProgram::seal` -> `PreparedValueSourceSupport::new` -> each `retain_output` | unconditional lexical/import/implementation/selected metadata capture, even declaration-only cells |
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
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts --expected-count 1
```

Discover exact affected production/test labels through the admitted configured
entry point before compiling; linking alone is not execution. No artifact or
executed count exists for campaign changes yet.

## Next actions

Finish the bounded issuer tranche, independently review its complete base-to-tip
delta and production consumers, route repairs, then fold full history to the
area branch. Settle census and exact transition identity before broad P4 or wire
consumers migrate. Continue dependency-ready 02 and 03 waves only with their
required reviewed contracts, retaining warm structural costs across request,
sealing, publication and first next request.
