# Checked value row candidate

Task: `/root/warm_compiler_campaign/owned_inputs/rows`.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.
Branch: `feature/shared-warm/owned-rows`.
Status: implemented, formatting checked; production/test compilation and execution unqualified. Exact candidate OID is delivered to the supervisor and reviewer after committing this handoff.

## Ownership and production route

`capture_produced_types` owns new mutable output observations. Each produced output has one private `CheckedValueRowIssuer` coupling its exact captured certificate to a cancellable promotion slot; consumers derive the certificate from that owner rather than pairing an arbitrary value and mutex. Failed promotion leaves the slot empty. `capture_output` validates the completed context, borrows or issues the exact value row through `RetainedArtifactMaterialization::capture_checked_value`, then invokes existing value/source/template support narrowing. Promotion happens before physical projection so an old broad row cannot remain the selected value backing. Host-built interfaces follow the same ordering.

The existing ArtifactView materialization cache now distinguishes `Compilation` from `CheckedValueLeaf(ArtifactBinding)`. Value leaves augment inherited/fixed custody. Complete materializations retain their prior ancestor suppression. Physical collection visits parent facts before new leaf facts, including through diamonds; leaf issuance never reconstructs compiler metadata, namespace, group permissions or home census. Same-view cold preparation includes its own leaves. Existing complete cache reads overlay newly issued selected leaves with an independent current-fact owner. Initial detachment uses the existing exact physical selection once; later revisions share persistent row-map structure and immutable scope/group/graph owners without retaining predecessor complete owners. In-flight complete owners remain unchanged.

Every ValueInterface row, including one first emitted by the generic original materializer before P6, is issued by the same `RetainedArtifactRow::issue_value` factory from its acquired ArtifactEntry. Values are excluded from both shared arena append and shared directory writing, and the generic path retains its existing mutable package validator/budget. A value leaf owns its own directory and sealed hi/package arena. Exact stored issuing facts plus the current selected binding/acquisition permit P6 to borrow an existing dedicated row with zero new factory work. Immutable fixed/source-map physical leases can therefore retain an old value without pinning unrelated value siblings. The existing DurableFiles fixture route uses dedicated packet child directories and preserves caller-owned packet cleanup. Closed custody variants prevent packet rows from issuing independently retained checked values; only exact Private issuing facts permit borrowing. Normal runtime directories remain independently owned. Semantic requirements remain with the certified value and exact interface row. A checked value retains that row and its selected source/template/native support instead of the producing output staging directory. `capture_checked` preserves duplicate/missing/excess certificate and requested-byte mismatch refusals, then borrows the selected original row/path. Import authority retains the complete checked artifact and compares the certified interface, while mutable raw fixtures retain filesystem authentication. Fresh staging stays separate.

`ExactScope.bindCapturedPurposeValues` already aliases the purpose path to the admitted checked-value row by typed owner and seal. Existing request materialization and `input_transport_files` own actual sealed descriptors through the existing endpoint close owner. There is no wire-schema or endpoint change. Retained diagnostics distinguish selected sealed transport metadata from the separately observed alias file, preserving expected bytes and existing observation-error fields. Optional Original roots, required historical children, native decisions, and acquisition overrides are untouched.

Strong checked artifact equality retains exact interface/path, row owner identity, producer/receipt, certificate, compiler support, source/template evidence and original prototype. Ordinary and Prepared captures of one produced output share one promotion. Independently issued equal bytes retain distinct physical issuers.

## Counters

The shared factory traces fresh row count, arena bytes and measured read/write/hash work across generic materialization and P6; packet attachment separately records borrowed versus freshly issued rows. Checked baseline capture traces selected references and zero baseline copying/transport reauthentication. Identical issuer-owned Arc buffers bypass byte comparison; separately supplied byte vectors retain explicit byte mismatch authentication and trace unshared comparison work. Existing `CheckedInputWork` now reports zero baseline files/bytes for certified borrowing, preserving raw fixture work. `selected_rows` traces actual owner/row/metadata visits; `input_transport_files` traces selected rows, slices, metadata entries, graph references and exported files. The leaf lookup traces actual scoped cache entries visited. These expose remaining enumeration; same-view hot lookup still scans scoped leaf/cache references, and this parcel does not claim constant metadata work or removal of Resident hit enumeration/encoding.

## Checks and deferred commands

Pinned formatting command (exit 0):

```sh
swarm-build bash scripts/dev-shell.sh rustfmt --edition 2021 tidepool/toolchain/src/checked_cell.rs tidepool/toolchain/src/declaration_context.rs tidepool/toolchain/src/artifact_inventory.rs tidepool/toolchain/src/host_work.rs
```

`git diff --check` passes. Evidence directory: `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/owned-inputs/rows/`; latest formatting logs `format-value-factory.log` and `format-packet-custody.log`.

The new checkout has a real registered bind mount at buck-out and initialized shared workspace submodule `ce60f76888c94e9d7229615f27762d02933549b1`. Earlier admission was blocked at `108249284608` available bytes. A later measured resource event reported `196494843904` available bytes, making the cold lane admissible. Pinned configuration completed exit 0 (`configure-value-factory.log`), and `swarm-build just check tidepool-toolchain tidepool-runtime` is in progress (`check-value-factory.log`, build ID `dcf3860c-467e-4764-863b-7fc5e923f64d`). The new checkout daemon PID3846846 is in `/build.slice/inanna-build-1792399526-3846769.scope`; observed build.slice MemoryCurrent23913897984 and host available86GiB. No compilation or test pass is yet established. No shared daemon was restarted or output deleted; no toolchain configuration was copied. The installed launcher does not support staged diagnostic/storage-status options; an earlier diagnostic formatting invocation refused before running its command (exit2).

Pending after configuration/materialization and measured disk admission:

```sh
swarm-build bash scripts/buck2-configure.sh --tests
swarm-build just check tidepool-toolchain tidepool-runtime
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::checked_baseline_borrows_issued_rows_and_releases_actual_backing --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::original_value_materialization_is_dedicated_and_reused_after_sibling_drop --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::concurrent_produced_value_capture_shares_one_promoted_row --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::same_view_cold_materialization_borrows_its_own_produced_leaf --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::same_view_many_promotions_keep_one_current_physical_frontier --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::tests::fixture_delivery_transfers_materialization_cleanup_to_packet_owner --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::tests::retained_materialization_prefix_a_b_a_borrows_only_unchanged_entries --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::produced_value_promotion_refuses_a_context_selecting_another_output --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::cancelled_produced_value_capture_leaves_promotion_empty_and_retries --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact host_work::tests::cancelled_owner_wait_refuses_before_the_holder_releases --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::checked_baseline_keeps_distinct_equal_byte_issuers_and_refuses_mismatch --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::checked_value_leaf_preserves_preexisting_dependency_rows_through_next_request --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact checked_cell::tests::final_value_capture_reuses_private_output_bytes_and_certificate_after_paths_are_removed --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts --expected-count 1
```

No counted test execution has yet been established; the native check includes directly affected test consumers. The integrated production/actor/shared-workspace workflow, Haskell consumer compilation, absence-override controls and actual runtime binding consumer refusal still need joined qualification at the folded candidate.

## Review and retention limits

Independent exact-candidate re-review is required before folding. Review the row-owner and complete/leaf cache change cumulatively, current-context promotion ordering, same-produced authority equality, original materializer dedicated V versus large sibling backing and V-only continuation with its source view/requests still alive, and preexisting dependency A row reuse through the next request.

Required native offers can intentionally retain their original custody, including shared backing. Original host-binding prototypes retain their original context. This parcel does not claim those independent owners or GHC/worker caches are collectible. Every value row is value-sized; arbitrary already-required non-value dependency rows may still own broad historical backing. Dedicated rows increase per-value memfd/directory counts. The existing 4096 arena/image transport bounds remain unchanged; no FD-scale acceptance or limit increase is established. Raw None output capture has no produced packet shared-promotion owner; it is the existing single final-capture route, and repeated alternative captures are not advertised as one physical issuer.

No release, deployment, NativeLink acceptance, performance improvement, or executed retention result is established. Next event: deliver exact commit to the supervisor and reserved independent reviewer; repair findings, then compile/run when a measured lane is available.
