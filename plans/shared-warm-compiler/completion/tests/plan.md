# Port behavioral tests to the sole compiler service

Read [the completion plan](../plan.md) and [locked contracts](../contracts.md)
first. This leaf owns test migration and its evidence, not native selection,
service output issuance, runtime publication or lifecycle policy. Base audit:
`6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`, code tip `92fa518105`. Record the
actual published plan/base OID at kickoff. No new execution was performed for
this plan.

The result is one production compiler route exercised by component and hosted
histories, with obsolete fixture authority deleted. Do not create a replacement
test engine, certifier, merge algorithm, recovery interpreter or generic fixture
framework. A successful finite/legacy run is historical evidence only. The first
joined history and early Persona gate precede suite expansion; complete mandatory
qualification still follows.

## Ownership and dependencies

Work in an isolated implementation checkout. Read the nearest guides for each
owning crate. The verification integrator receives this leaf's reviewed commits.
Coordinate these concrete boundaries before modifying shared files:

| Dependency owner | Contract received by tests | Test work to forward |
|---|---|---|
| Native selection | Exact root/dependency/current-product demand; required publication closure and current projection | Focused deterministic regressions and independent model histories. Send proposed error variants/refusal controls to that owner. |
| Service cells | Actual world plan, executable checked output, transaction origin and settled compiler result | Small production-generated cohorts; output/continuation histories. Forward narrow changes in `artifacts::test_support` or service-owned test modules. |
| Runtime publication | Sole ordinary cell entry, item settlement and atomic public successor | Hosted cells and route observations. Runtime owner receives shared admission/publication changes before consumers depend on them. |
| Lifecycle | Real original A/target B admission, capture ownership, retirement/recovery | Lifetime, lost-waiter, refusal and recovery scenarios using the same issued outputs. |
| Queries and qualification | All-verb semantic results, frozen production bundle, declared trace completeness | Shared hosted entrypoints and early Persona evidence; final commands and bundle identity. |

Keep each forwarding commit narrow: base/head, owned paths, dependency OIDs,
consumer, exact command and execution result. Do not land a test factory first
and leave its real producer for a later parcel. Compile the owner test target and
direct consumer tests after shared API changes, then integrate a representative
joined history. Review the complete base-to-tip delta. Tests do not authorize an
implementer to rewrite another owner's selection or source-admission policy.

## Structural migration and deletion

Extend existing `artifacts::test_support`, proptest replay drivers,
`TestCampaign` and `HostedTestRuntime`. Production wrappers must enter the same
new service implementation as ordinary execution; a wrapper around the old
finite compiler is not a migrated producer. Remove forced-legacy selections,
fallback enum arms/getters, environment choices and test-only callers with the
runtime cutover. Warm reuse and cold reconstruction must restore the same logical
world, never choose an old engine.

The runtime-publication owner deletes `PrivateExecutionCompilerBasis::Finite`,
`RuntimeCellCompilerBasis::Finite`, `RuntimeFiniteCellInputs`, `finite_inputs`
and dependent planned-row/compiler-view getters in `session/admission.rs` after
their consumers are ported. This leaf supplies the migrated tests as narrow
commits to that owner rather than editing those shared definitions independently.
Test startup, rendering and recovery compiler work must also use the service;
their legitimate runtime purposes do not retain finite compiler branches.

| Remove after porting its valuable callers | Production replacement |
|---|---|
| `ArtifactView::merge_fixture_selections` (`artifact_inventory.rs`) | `CompilerSourceSupport::merge` and its issued compiler-role/native-decision merge; custody-only cases use the existing custody owner. |
| `issue_fixture_compiler_support`, `recover_fixture_compiler_support`, `recover_fixture_projection`; wrappers `fixture_compiler_support` and `fixture_support_with_issued_offers` | Real service-issued context/support, followed by owner-issued narrowing, handoff and recovery. |
| `CompilerSupportFixture::{Complete, RecordedRoles}`, `support_offer`, `recorded_role_support_offer`, `admit_fixture_support`, `admit_fixture_segment_support` | Actual checked world-cell output and ordinary admission of its sealed products. |
| Test-only `RecoveredArtifactInventory::context_with_graph_selection` | Actual durable `Materializer` output, `hydrate_context`, `context_for_expected_snapshot`. |
| `CertifiedSourceSelection::from_projected_originals`; positive histories that manually assemble `admit_current`/`bind_produced_originals` | The production completion stage issues the selection and producing original identity together. |
| Positive behavioral uses of `original_groups_fixture*`, `fixture_finalized_product*`, `fixture_native_declaration_product`, `fixture_module_interface`, `recovered_witness_fixtures` | Small authored Haskell modules compiled through the new service, retaining the exact issued native receipts. Remove exported authority manufacturers after their remaining decoder tests are localized. |
| Positive checked-value setup through `capture_raw`, `prepare_test_value_interface_files`, `capture_test_value*` | Real ordinary cells produce checked values; actual capture, transfer and next consumption exercise authority. |
| Batch C's proposed `transported_roles_match_owner` comparator | Transport the actual durable owner; production authentication decides validity. Do not compare supplied roles and then restore an unchanged original snapshot. |

Trace callers before deleting a helper. Delete otherwise-unused production
escape hatches maintained only for its unsupported state. Keep production
internals such as `ExactDeclarationContext::from_facts`/`rebuild` where they still
implement the owner; remove behavioral tests' private-field access and synthetic
fact editing rather than duplicating those internals elsewhere.

Legitimate boundaries remain:

- Raw malformed packets are local decoder/authenticator inputs with a valid
  owner-produced control. They do not become positive `Certified*` execution
  authority. Pure representation tests may retain untrusted data constructors.
- Production durable writers/readers and certificate acquisition remain real
  component boundaries. Acquire bytes issued by the production producer; do not
  regenerate certificates to fit a desired graph.
- `world_context_fixture` is only tagged DATA for its existing Haskell reader
  test. Its actual Haskell owner must reseal it; it cannot replace runtime
  acquisition or be advertised as execution proof.
- `build_prepared_fixture` and corpus build actions retain their declared,
  pinned, portable-output contract. Prepared resources cannot be restamped as
  source-bound native certificates. This distinct build consumer does not
  preserve a second resident engine.

Completion of deletion requires source review showing no old resident selector,
fallback or fake authority ingress, and compilation of owning libraries, their
tests and direct consumers so missed callers fail. Do not add tests whose only
assertion searches code text. Review generated Buck mappings after fixture
changes; test-only files must not invalidate unrelated production/browser inputs.

## The 32 recorded failures and their obligations

Evidence: `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/final-join/TOOLCHAIN-SWEEP.md`
and `reviews/ADVERSARIAL-TRUNK-RED-{A,B,C}.md` under the same campaign root.
The sweep ran at `75476fb97b`, selected 1116, executed 1086, passed 1054,
failed 32, ignored 30, exit 1. Changes from that code to the audited HEAD are
Markdown. These are historical measurements, not results of this plan. Numbers
below preserve the sweep's IDs; use the full names for runner selections.

Each row needs a final disposition: repaired product; ported behavioral test;
localized raw corruption test; or obsolete test removed with its distinct
obligation covered elsewhere. Record the replacement test and producer. A
first panic does not classify later assertions: rerun the complete repaired
history and retain every deeper failure it exposes.

### Inventory projection, model and cost: 12 cases

Owner: `artifact_inventory.rs`, `artifact_inventory/compiler_projection.rs`,
`compiler_projection_properties.rs`, `native_history_properties.rs`,
`view_read_properties.rs`.

| # | Full test name | Disposition and preserved obligation |
|---|---|---|
| 1 | `artifact_inventory::compiler_projection::tests::closed_interface_narrowing_preserves_rows_without_output_metadata` | Retire obsolete metadata-cache sensitization. Keep row/binding/selection equality and verify both construction paths leave optional output metadata cold. |
| 2 | `artifact_inventory::compiler_projection_properties::exact_group_availability_survives_projection_clone_and_cross_inventory_merge` | Port merge to issued production support. Opposite-role custody must not select execution. The later test-only namespace-union failure is not a demonstrated production merge defect. Check both merge orders, exact group/carrier edges and next target consumption. |
| 3 | `artifact_inventory::compiler_projection_properties::same_view_a_b_a_materialization_reuses_only_the_exact_projection` | Compile self-contained compatible versions. Retain A/B/A emitted bytes, exact keys and final release; keep incompatible-version consumption as a typed refusal. |
| 4 | `artifact_inventory::native_history_properties::projected_complete_carrier_custody_matches_minimized_history` | Correct and port the independent physical-custody model; retain the minimized history and exact alternate issuers. |
| 5 | `artifact_inventory::native_history_properties::scoped_carrier_namespace_controls_projection_and_recovery` | Same model, including scoped namespace choice followed by actual recovery and consumption. |
| 6 | `artifact_inventory::native_history_properties::staged_future_interface_does_not_widen_prior_views` | Same model; later interfaces must not alter prior published views or their authority. |
| 7 | `artifact_inventory::native_history_properties::staged_native_histories_match_raw_fact_model` | Model fresh root reissuance separately from parent-binding custody. Remove fake merge. Preserve shrinking, exact identities/edges and extra-result checks; current residual cause is unresolved. |
| 8 | `artifact_inventory::tests::bound_monotone_group_history_only_visits_new_closure` | Preserve a read-cost law: measure fresh-read work separately and bound it by newly reached closure, then require repeated read/admission to add no work. |
| 9 | `artifact_inventory::tests::bound_segment_preserves_explicit_groups_and_same_carrier_ambiguity` | Non-explicit demand may consume its carried choice. Assert the exact expected `NativeGroupBinding`, not a nonempty ordinal subset; preserve real competing-choice refusal. |
| 10 | `artifact_inventory::tests::empty_admission_and_parent_only_release_do_not_scan_history` | Isolate empty admission metrics before a real merge; preserve no-history-scan and release behavior. Measure merge cost independently. |
| 15 | `artifact_inventory::view_read_properties::empty_observation_preserves_inherited_roots_and_separate_native_custody` | Retire lease pointer equality; compare roots, namespace, exact selected groups, custody roots and readable owned bytes. |
| 16 | `artifact_inventory::view_read_properties::same_inventory_merge_keeps_custody_after_peer_read_caches_drop` | Parent leases/read caches may release. Merged exact custody, exhaustive retained closure and final payload/node release remain required. |

Batch B `a67c933ed0` is source material, not merge-ready. Salvage #1, #3, the
direction of #4-6's custody widening, receiver-inventory attribution and #16's
self-contained owner correction. Reject #8's deleted read bound, #9's subset
oracle, #10's combined measurement and #15's omission of native custody. For #7,
`parent.facts().contains(Group(node, ordinal))` erases physical scope and cannot
justify exact inherited custody. For #2, `merge_fixture_selections` is not
`merge_compiler_inputs_inner`/`retain_merged_selection`; do not promote its
failure to a product finding. The reported ten passes exclude two red cases.

### Exact carrier selection: two cases

Owner: existing native demand/verified-binding owners, with native-selection leaf.

| # | Full test name | Disposition and preserved obligation |
|---|---|---|
| 11 | `artifact_inventory::tests::independent_native_roots_keep_overlapping_exact_descendant_bindings` | Two real roots keep their separately issued helper edges. Direct ambiguous helper demand refuses; a historical child is resolved through its parent's authenticated edge. Keep recovery, next bound target and unchanged state on refusal. |
| 13 | `artifact_inventory::tests::native_variants_keep_exact_carriers_and_require_explicit_materialization_selection` | Separate selected choice/ambiguity from broad historical payload custody. Preserve explicit materialization choice and continuation roles; do not fix by dropping historical bytes. |

The current metadata census counts retained native variants as owner ambiguity;
that conflation is source-visible. A product repair must also preserve
`continuation_namespace_preserves_roles_without_promoting_retained_bodies`.
Require a single foreign compatible candidate refusal, not only a multiple-choice
control. Absence of permission cannot choose the sole available group.

### Availability/dependency issuance: three cases

Owner: `NativeInputSelection::normalize_issued_contributions`, real candidate
completion and exact dependency issuance, not another authority factory.

| # | Full test name | Disposition and preserved obligation |
|---|---|---|
| 12 | `artifact_inventory::tests::late_native_original_recapture_keeps_inherited_roots_and_historical_children` | Carry the helper's existing qualified contribution, independent of executable roots. Preserve inherited roots and both historical child bindings through recapture, recovery and preparation. |
| 14 | `artifact_inventory::tests::private_original_requirement_survives_receipt_with_interface_only_children` | Replace sibling fixture merge with actual request support. Retained sibling custody cannot fabricate the root's private dependency permission. Keep exact dependency/ordinal refusal and successful receipt continuation. |
| 25 | `declaration_context::native_availability_tests::fresh_candidate_dependency_uses_issued_availability_without_selecting_execution` | Port direct `CandidateSet` construction to real candidate completion. Custody-only candidates earn no offer; a completion explicitly authenticating an accepted cached product may carry its real issuer. Preserve availability versus demand and reject unrelated-output promotion. |

Case #25's current absence of a Full offer is source-visible, but the handmade
candidate does not establish a production capability loss. The locked contract
settles the policy: no blanket restoration of the removed candidate issuer.

### Completion/continuation: five cases

Owner: `certified_products.rs`, `certified_products/tests/native_request_history.rs`
and actual service completion; downstream `OriginalCompilerInputs` consumer.

| # | Full test name | Disposition and preserved obligation |
|---|---|---|
| 17 | `certified_products::tests::completed_request_native_owners_survive_historical_custody` | Real issued continuation; valid retained control, precise missing-original refusal, historical custody and next consumer. |
| 18 | `certified_products::tests::continuation_namespace_preserves_roles_without_promoting_retained_bodies` | Same completion boundary, distinct obligation: no role/execution promotion from retained bodies. Retain this control against broad ambiguity fixes. |
| 19 | `certified_products::tests::native_request_history::generated_private_transport_histories_preserve_issued_graphs` | Generate actual produced-original completion, private handoff and subsequent demand/recovery, with exact issuer identities. |
| 20 | `certified_products::tests::native_request_history::private_transport_preserves_independent_closures_until_exact_demand` | Deterministic independent-closure control using the same real producer; distinguish custody, availability and demand. |
| 24 | `declaration_context::native_availability_tests::completed_request_carries_exact_entries_without_reconstruction` | Preserve actual carried entry and physical owner, no reconstruction, foreign equal-byte binding control and final release. Correct the stale first error check, then run all later assertions; later custody failure remains unresolved. |

Add bounded typed `CertificationError::MissingRetainedCustody { artifact }` at
the missing-original lookup in `ReceiptNativeSelection::original_entry` (final
name belongs to the owner). Assert the expected ID saved from the valid issuer.
Keep wrong binding/owner, missing canonical custody and unbound produced identity
distinct where tested; reuse existing typed conflicts or add a narrow cause.
An additive variant does not require rewriting every `Mismatch` site. Update
exhaustive error consumers, including `failclass.rs` and `recovery_artifacts.rs`,
and retain the meaningful cause at reporting boundaries. Do not classify every
missing-key failure as a successful authority refusal.

### Publication/projection/recovery: seven cases

Owner: `declaration_context/published_source.rs`,
`artifact_inventory/compiler_projection.rs`, actual service publication and
durable restoration. Coordinate with native-selection/runtime publication.

| # | Full test name | Disposition and preserved obligation |
|---|---|---|
| 21 | `certified_products::tests::publication_request_history::generated_publication_support_and_recovery_preserve_issuer_selection` | Real publication, durable transport, restored birth proof custody, later demand and dependent output. |
| 27 | `declaration_context::published_source::history_properties::publication_reopening_refuses_incomplete_lexical_type_namespace` | Preserve immutable birth; missing required current lexical/type roles refuse, unrelated withdrawal succeeds. Localize deliberate corrupt durable input. |
| 28 | `declaration_context::tests::merkle_context_history_matches_canonical_and_cold_facts` | Replace private rebuild/fixture projection with owner-issued withdrawal. Compare incremental/cold writer results and independently saved contents; consume restored narrowed support. |
| 29 | `declaration_context::tests::program_support_scopes_published_roles_to_exact_carriers_without_retiring_parent` | Narrow actual returned support, consume/recover it, and preserve parent usability. Establish exact expected interfaces rather than a separately manipulated fake view. |
| 30 | `declaration_context::tests::published_original_manifest_binds_native_selection_and_restored_policy` | First publication retains required closure only; preserve selection/policy identity and flat/altered-custody refusals. |
| 31 | `declaration_context::tests::published_original_retains_lexical_only_interfaces_without_native_offer` | Lexical/type-only requirement remains canonical, without selecting its available implementation. Preserve instance/family/orphan obligations independently. |
| 32 | `declaration_context::tests::published_original_selection_refuses_unissued_root_and_custody_loss` | Unrelated available/private roots stay excluded. Unissued roots and genuine custody loss refuse; no ambient inventory reconstruction. |

First-publication fallback currently captures ambient support; Root E addresses
the right owner, but its broad group-custody and InterfaceOnly rules are not an
accepted parcel. Forward a narrow repair against the locked required-closure
contract. Cases #28-29 remain unresolved narrowing/consumer boundaries.

Batch C `1c3eaa8ab4` is not merge-ready. Salvage the finding that production
binds pending current products before projection, but replace the manual fixture
sequence with actual completion. Salvage correct capture-directory measurement
and consider `PublicationNativeAuthority::bind`'s proof-scope restoration as a
separate production fix: `publication_birth_support` already requires the saved
snapshot to equal the recorded birth. Qualify it through real durable restoration.
Reject the shared `Mismatch` string oracle, exact-group erasure and transported
role comparator. Comparing altered roles then restoring `context.snapshot()`
does not prove a successful transported-owner path. Seven isolated green
selectors do not qualify the joined product change.

### Checked capture: two cases; materialization: one case

| # | Full test name | Owner and preserved obligation |
|---|---|---|
| 22 | `checked_cell::tests::independently_captured_values_preserve_one_compiler_native_issuer` | `checked_cell.rs` and support merge. Real values share one selected issuer; compare full `NativeGroupBinding`, including selection. The divergent control must demonstrate exact conflicting choices before merge. |
| 23 | `checked_cell::tests::repeated_template_values_keep_exact_custody_and_private_permissions` | Real capture and retained-row owners have separate directory lifetimes. Assert each positive lifetime and final release; do not measure a row directory as capture scratch. |
| 26 | `declaration_context::native_availability_tests::issued_offer_retention_preserves_materialization_parent_precedence` | `retain_issued_offer_custody`/materialization. Preserve selected physical facts, operand precedence, exclusion and directory release. Determine whether one complete parent is equivalent to four eager parents before retiring an incidental count. |

Case #22's proposed `NativeGroupKey` comparison drops the issuer selection, so a
wrong exact binding passes. Adding an unrelated artifact does not prove the
ambiguity control reaches a genuinely competing group choice. Case #26's one
versus four parents is unresolved: retain the eager reference as investigation
evidence, and decide from selected files/ownership rather than changing the count.

## Independent histories and economical real producers

Extend existing operation enums/replay drivers. Model payload identity, physical
issuance, original carrier, canonical binding, immutable birth, current compiler
role/root, custody, availability, executable demand and lifetime independently.
Generate equal payloads in distinct issuances and overlapping historical edges;
generate available-but-unselected and retained-without-grant cases deliberately.
Keep a slow authored-graph fixed-point oracle, not production traversal or
normalization copied into expected results.

Observe opaque handles at issuance only to establish checked correspondence.
Assert uniqueness before storing that mapping; an alias must not overwrite the
model. Expected membership and edges derive from logical operations. In particular,
fresh projected roots and retained parent bindings must remain separate physical
rows; logical `(module, ordinal)` equality is insufficient. Test both missing and
extra facts. Reads are explicit operations when they warm caches. Preserve the
existing shrinking/persistence configuration and emit minimized operations,
premises, successful transitions and refusal coverage even on failure.

Compile a small authored cohort through the sole service once within an owning
test process, retain its actual immutable outputs/receipts/source owners, and
replay many histories against fresh mutable inventories, worlds and stores.
Use a few controlled source versions and symbols; derive ordinals from the
issuer's authenticated product while keeping expected dependency relationships
authored independently. Include a deterministic causal history beside each
confirmed property defect. Calibrate a bounded meaningful mutation in an isolated
owned checkout and prove the property shrinks/persists/replays it.

The native runner isolates test cases in separate processes. Do not promise a
process-global cohort shared across unrelated test functions, serialize mutable
test worlds, add an invocation memo or restamp portable artifacts. Consolidate
duplicated scenarios into an existing replay test when they have the same
obligation; keep independent boundary controls distinct. Measure setup compilation,
callback count, meaningful histories, reads and recovery work before expanding
campaign size. No new global cache/framework is justified by repeated setup.
Extend `artifacts::test_support::with_settlement` only as needed to retain the
actual submitted operation and close observations: its current `all(clean)`
check is vacuous for an empty observation list. A submitted producer scenario
must establish its real request/origin and matching terminal observation; an
explicit refusal-before-submission control instead proves no request was issued.

## Joined execution, early Persona and final qualification

1. Compile the smallest changed owner test target and its direct consumers.
   Replay one real imported-module cell followed by a later cell using its
   published successor. Retain actual world/operation/worker/output identities,
   settled END and runtime settlement. Extend promptly to declaration, explicit
   and empty import, expression, mixed cell, source-defined retained type and
   later reuse. Integer output alone is a debugging milestone.
2. Use the existing hosted route: `HostedTestRuntime::start_prepared_configured`,
   `hosted_script_provider` and `TestCampaign` dispatch. Extend
   `ordinary_demand_history_tests::prepared_actor_entry_late_binding_and_warm_repeat_observe_actual_work`
   and the existing startup Persona/shared-workspace scenarios. A prepared host
   name or `owned-resident` mode label is not route evidence: join actual worker
   requests, held-world transitions, produced native receipt, item settlement
   and public successor. Delete old finite setup/selectors before claiming this
   gate. Run a successor after predecessor realization eviction and disposable
   operation-output removal, keeping immutable source/native ownership.
3. With queries, execute the five public discovery verbs and assert parsed
   semantic fields, provenance and stale-view refusal. The current
   `IntrospectionFrontDoor.hs` executes only `result = pure 43` in
   `introspection_front_door_prepared_result_executes`; retain that as portable
   execution evidence, not handler execution. Run the actual composition through
   hosted Haskell, then invoke discovered behavior. Diagnose A1c at its exact
   received native dependency choice without widening inventories.
4. Replace second-cell telemetry's `any(compiled_off_checkout)` oracle with
   evidence attached to that exact second cell and its real split/compile owner.
   A constant true on certified install cannot issue compile provenance.
   Preserve a real execution/settlement witness and a control that detects
   fallback; delete the fallback production route rather than selecting it in
   the positive test.
5. Meet G2 in the completion plan: production-profile frozen bundle, real Persona
   source/spec/memory/tools, hosted inference through actual Haskell/handlers,
   all required ordinary cell shapes, query semantics, successor reconstruction,
   stale/revoked/cancelled/uncertain refusal, unchanged old public state and
   truthful effects/cleanup. Then the qualification owner runs a bounded real
   DeepSeek smoke and launches the fresh Persona conversation from that exact
   bundle. Do not defer this until the final performance matrix. Unfinished
   force behavior explicitly refuses; advertised ordinary defects block G2.
6. Finish G3: forks, force/default retirement, source A/target B, cold recovery,
   worker replacement, disappearing waiters, historical exact edges and physical
   release. Close every table row by actual behavior/disposition, run affected
   native suites and compiler fixtures, then bundle-owned M1-M3 and authored
   Persona histories on one exact final bundle. Hand timing/work measurements
   to qualification; local component passes do not qualify that bundle.

## Commands and evidence

Run as `inanna`, verify the checkout's `buck-out` bind mount and resource
headroom, and follow `docs/swarm-builds.md`. Configure materializes pinned inputs:

```sh
swarm-build env TIDEPOOL_BUCK_CACHE=false TIDEPOOL_BUCK_REMOTE=false bash scripts/buck2-configure.sh --tests --browser
swarm-build just check tidepool-toolchain tidepool-runtime exomonad-actor exomonad-host
swarm-build just test-list tidepool-toolchain lib
swarm-build just test-lib tidepool-toolchain --exact artifact_inventory::tests::independent_native_roots_keep_overlapping_exact_descendant_bindings --expected-count 1 --jobs 1 --timeout 900
swarm-build just test-lib tidepool-toolchain --exact certified_products::tests::native_request_history::generated_private_transport_histories_preserve_issued_graphs --expected-count 1 --jobs 1 --timeout 1800
swarm-build env PROPTEST_CASES=128 just test-lib tidepool-toolchain --exact certified_products::tests::publication_request_history::generated_publication_support_and_recovery_preserve_issuer_selection --expected-count 1 --jobs 1 --timeout 1800
swarm-build just test-lib exomonad-host --ignored --exact actor_host::ordinary_demand_history_tests::prepared_actor_entry_late_binding_and_warm_repeat_observe_actual_work --expected-count 1 --jobs 1 --timeout 1800 --compiler-mode owned-resident --output-dir "$ORDINARY_DEMAND_EVIDENCE" --retain-artifacts --trace-profile full
```

These are starting commands for existing names, not an assertion that today's
cases already enter the new engine. Discover changed names with the owning
runner and retain mappings from retired cases. Set `ORDINARY_DEMAND_EVIDENCE` to
a fresh private absolute evidence directory and record it before the command.
The 128-case property run is initial feedback; expand after measuring real setup
cost and history coverage, and retain actual callbacks rather than inferring them
from the environment variable. `just check` is an incremental
joined compile set, not a substitute for compiling changed test targets. Native
recipes issue `--local-only -c remote.enabled=false`; verify the effective
configuration/logs retain those settings and do not report the cache-mode wrapper
dropping them. Keep ignored matched-compiler cases explicit; missing compiler or
browser input is a failure, not a passing skip. The ordinary-demand case requires
the explicit full trace profile above; prove its applied record before drawing
absence conclusions. Do not use that instrumented run as unqualified minimal-trace
latency evidence.

After focused changes pass, run the owning module/cluster and direct consumers,
then the complete toolchain lib suite once the migrated rows are integrated:

```sh
swarm-build just test-lib tidepool-toolchain --jobs 1 --timeout 1800
swarm-build just fixtures-check
```

Add explicit `--ignored --exact ... --expected-count N` selections for required
compiler/host cases excluded by an ordinary sweep. Use the owning native Haskell
targets for changed compiler contracts, discovering exact labels in the admitted
environment. Do not run broad `just verify` as a spot check. Production-profile
commands and exact frozen bundle qualification belong to the qualification plan;
evidence for one profile cannot qualify another profile's bytes. Here a production
path may use the recorded `fast-dev` deployment profile; it means the real prepared
catalog/service/tool route, not an invented profile named `production`. Admit more jobs only from
measured aggregate memory/cgroup/scratch headroom, not CPU count.

For every declared result retain base/head, dependency commits, exact command,
effective profile and pinned compiler/worker/stdlib/browser identities, artifact
hash, selected/executed/passed/failed/ignored counts, exit status and log/report.
Property evidence additionally records configured fresh cases, actual callbacks,
successful/refused transitions, minimized traces, seed and replay. Linking,
listing, generated callback setup and a zero-observation/zero-match selection are
not execution passes. Run pinned formatting and `git diff --check` for implemented
changes. Qualify the exact received code after integration, not the author's
earlier artifact.

## Remaining uncertainty and escalation

This is a source/evidence plan. It does not resolve the current #7 fresh-selection
model gap, #24 later custody failure, #26 materialization equivalence or #28-29
narrowing defects. Several alleged product failures still enter test-only
constructors. Preserve their minimized inputs and reach the real producer before
classifying them. The new service has no production ordinary caller at the audited
base and lacks required settlement/publication behavior; no current hosted pass
establishes G1/G2. Send any missing issuer operation to its owning leaf instead
of manufacturing authority to make setup pass. Escalate only an actual contract
conflict with its smallest counterexample; continue independent ports meanwhile.
