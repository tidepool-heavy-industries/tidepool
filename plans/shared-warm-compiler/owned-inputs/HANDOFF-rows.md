# Checked value row candidate

Task: `/root/warm_compiler_campaign/owned_inputs/rows`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.
Branch: `feature/shared-warm/owned-rows`.
Status: implemented, formatting checked; production/test compilation and execution unqualified. Exact candidate OID is delivered to the supervisor and reviewer after committing this handoff.

## Ownership and production route

`capture_produced_types` owns new mutable output observations. Each produced output has one packet-local cancellable `Mutex<Option<Arc<RetainedArtifactRow>>>`; failed promotion leaves it empty. `capture_output` validates the completed context, promotes only the new value through `RetainedArtifactMaterialization::capture_checked_value`, then invokes existing value/source/template support narrowing. Promotion happens before physical projection so an old broad row cannot remain the selected value backing. Host-built interfaces follow the same ordering.

The existing ArtifactView materialization cache now distinguishes `Compilation` from `CheckedValueLeaf(ArtifactBinding)`. Value leaves augment inherited/fixed custody. Complete materializations retain their prior ancestor suppression. Physical collection visits parent facts before new leaf facts, including through diamonds; leaf issuance never reconstructs compiler metadata, namespace, group permissions or home census.

A value leaf owns its own directory and sealed hi/package arena. Semantic requirements remain with the certified value and exact interface row. A checked value retains that row and its selected source/template/native support instead of the producing output staging directory. `capture_checked` preserves duplicate/missing/excess certificate and requested-byte mismatch refusals, then borrows the selected original row/path. Import authority retains the complete checked artifact and compares the certified interface, while mutable raw fixtures retain filesystem authentication. Fresh staging stays separate.

`ExactScope.bindCapturedPurposeValues` already aliases the purpose path to the admitted checked-value row by typed owner and seal. Existing request materialization and `input_transport_files` own actual sealed descriptors through the existing endpoint close owner. There is no wire-schema or endpoint change. Optional Original roots, required historical children, native decisions, and acquisition overrides are untouched.

Strong checked artifact equality retains exact interface/path, row owner identity, producer/receipt, certificate, compiler support, source/template evidence and original prototype. Ordinary and Prepared captures of one produced output share one promotion. Independently issued equal bytes retain distinct physical issuers.

## Counters

Actual row issuance traces fresh row count, arena bytes and measured promotion read/write/hash work. Checked baseline capture traces selected references and zero baseline copying/transport reauthentication. Identical issuer-owned Arc buffers bypass byte comparison; separately supplied byte vectors retain explicit byte mismatch authentication and trace unshared comparison work. Existing `CheckedInputWork` now reports zero baseline files/bytes for certified borrowing, preserving raw fixture work. `selected_rows` traces actual owner/row/metadata visits; `input_transport_files` traces selected rows, slices, metadata entries, graph references and exported files. These expose remaining enumeration; this parcel does not claim Resident hit enumeration or encoding has been removed.

## Checks and deferred commands

Pinned formatting command (exit 0):

```sh
swarm-build bash scripts/dev-shell.sh rustfmt --edition 2021 tidepool/toolchain/src/checked_cell.rs tidepool/toolchain/src/declaration_context.rs tidepool/toolchain/src/artifact_inventory.rs tidepool/toolchain/src/host_work.rs
```

`git diff --check` passes. Evidence: `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/owned-inputs/rows/format.log`.

The new checkout has a real registered bind mount at buck-out. `/srv/build` observed available bytes `108798550016` (about 101.3 GiB), leaving about 1.3 GiB above the required 100 GiB reserve. No admissible cold Buck build was established; no build/test was launched or called passing. The installed launcher does not support the staged diagnostic/storage-status options; an attempted diagnostic formatting invocation refused before running a command (exit 2), then ordinary admitted pinned formatting passed. No daemon was started, restarted, or deleted. No compiler toolchain configuration from another checkout was copied.

Pending after configuration/materialization and measured disk admission:

```sh
swarm-build bash scripts/buck2-configure.sh --tests
swarm-build just check tidepool-toolchain tidepool-runtime
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::checked_baseline_borrows_issued_rows_and_releases_actual_backing --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::promoted_value_leaf_releases_broad_sibling_backing_before_next_request --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::concurrent_produced_value_capture_shares_one_promoted_row --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::checked_baseline_keeps_distinct_equal_byte_issuers_and_refuses_mismatch --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::checked_value_leaf_preserves_preexisting_dependency_rows_through_next_request --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::final_value_capture_reuses_private_output_bytes_and_certificate_after_paths_are_removed --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts --expected-count 1
```

All selected/executed test counts are currently zero because these commands are deferred, not because a runner selected zero cases. The integrated production/actor/shared-workspace workflow, Haskell consumer compilation, absence-override controls and actual runtime binding consumer refusal still need joined qualification at the folded candidate.

## Review and retention limits

Independent exact-candidate review is required before folding. Review the row-owner and complete/leaf cache change cumulatively, current-context promotion ordering, same-produced authority equality, actual old broad V+large sibling arena destruction, and preexisting dependency A row reuse through the next request.

Required native offers can intentionally retain their original custody, including shared backing. Original host-binding prototypes retain their original context. This parcel does not claim those independent owners or GHC/worker caches are collectible. A value's own row is value-sized; arbitrary already-required dependency rows may still own broad historical backing. Raw None output capture has no produced packet shared-promotion owner; it is the existing single final-capture route, and repeated alternative captures are not advertised as one physical issuer.

No release, deployment, NativeLink acceptance, performance improvement, or executed retention result is established. Next event: deliver exact commit to the supervisor and reserved independent reviewer; repair findings, then compile/run when a measured lane is available.
