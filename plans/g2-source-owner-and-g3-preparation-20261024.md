# G2 SOURCE ownership repair and bounded G3 preparation

## Contract and delivery

Accepted baseline: `594f110a8b812c829814bc184d89d7c9a4e4f6c9`.
Integration: `integration/g2-domain-runtime-20261023`, checkout
`/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
The coordinator reviews and integrates concrete commits; workers use isolated
checkouts. Implementation, component execution, joined execution and frozen
qualification are separate states. No new G2/G3 pass is claimed by this plan.

The five actor notebook cases executed and failed at the group-46 boundary on
98d1d08198. SOURCE observation incorrectly asks passive custody about a group
retained by a Full native offer. Earlier runtime histories passed at their
recorded revisions, not this future candidate.

Five Sol implementation lanes: G2 Rust, G2 Haskell, Q9, Q10, Q6. Each owns its
production changes and meaningful positive/refusal tests, compiles affected
production and test consumers, and records source, commands, counts, exits,
cleanup and pending qualification. Read the nearest contributor guidance.
Use admitted pinned builds and registered scratch. Measure shared headroom;
do not use five fresh full builds by default or copy another checkout's config.
Tests must enter through real owners, not new test authority constructors.

## G2 Rust: authenticated SOURCE observation

Own artifact_inventory.rs, certified_products.rs, declaration_context.rs,
declaration_context/source_dependency.rs, and directly required Rust consumers.

- Move available_group to AuthenticatedOriginalBindings; retain delegating
  OriginalBindingSelection API and operation-owner/exact-pair/choice checks.
- SourceDependencyRow retains ChosenNativeGroupSupport. Resolve children,
  retained bindings and later payloads through that exact contribution, never
  passive CompilerSourceSupport::artifacts(). Observation grants no execution.
- Relocate existing SelectedOriginalGroupScope into artifact_inventory, opaque:
  selected-view uses selected_nodes for every query; acquired constructor checks
  custody, each query checks its explicit bindings, and child traversal requires
  completeness within that set; contribution uses its existing reachable set.
- Require exact Group and ArtifactBinding membership before payload access.
  Preserve has_home_owner's broader anti-package-downgrade check and recorded
  carrier pairs; never reconstruct carrier selection from group selection.
- Migrate restoration/certification consumers then delete all five passive-view
  acquired_original_group APIs and contains_acquired_native_group. Keep lower
  helpers private. Leave every ArtifactView::from_custody call unchanged.
- Fixture: production CapturedCompilerStage/cohort availability issuance plus
  independently established SOURCE admission; Full offer with parent -> child
  -> independently expected nonempty retained terminal. Prove passive-custody
  absence, exact offer membership, successful observation and terminal use,
  unchanged passive/execution selection. ScopeInterfaces alone does not issue Full.
- Refusals: partial offer, missing custody, outside SOURCE scope, competing
  unresolved issuers, and retained occurrence outside the selected contribution.
  Explicit acquired left_group remains legal with unrelated right_group custody.

## G2 Haskell: preserve typed failures and avoid repeated observation work

Own GhcPipeline.hs load/failure/dependency sections, DependencyPlan.hs and
RequestInputsTest.hs. Coordinate Q9's disjoint Finder predicate section and Q6's
obsolete RequestInputsTest import deletion. Do not add another hook wrapper.

- Extend existing canonicalFailureRef to a private canonical/native failure sum.
  Catch NativePreparationFailure inside existing captureCanonicalFailure.
  First native supersedes earlier canonical frontend; first wins within a tier.
  Preserve typedPreparationFailureRef/cancellation and typedFailureRef precedence.
  Rethrow typed failure before source-diagnostic fallback. Universal callback
  precedence and World-to-template fallback are not established claims.
- Consolidate existing MVar () + IORef map to MVar map; preserve serialization
  across discovery, query and publication plus freeze. This removes redundant
  state, not a demonstrated race in the existing locked implementation.
- Deduplicate actual free Ids across binds before dependency resolution. Index
  originals by Map Module (Set WorldOriginalReference), collapse identical refs,
  reject competing refs only on demand, and keep traceNativeUses observational.
- Regression uses existing withDiagnosticCountFault on fresh
  WorldCancellationNames load: inject distinctive NativeReplyCorrupt, assert
  fault fired, exact cause survived, sourceFailureDiagnostics is Nothing, and
  fresh compiler scope recovers. Keep transport and cancellation controls.

## Q9: honest discovery

Own Introspection.hs, IntrospectionSearchTest.hs and only the shared clean-Finder-
NotFound predicate in GhcPipeline.hs. Coordinate that narrow shared file change.

- Absence only for clean Finder NotFound; hidden/unusable/ambiguous providers and
  Found with missing ModuleInfo fail. Reject the whole reserved session namespace.
- Current checked target uses tcg_exports/tcg_type_env without requiring HMI.
- Relevant advertised Name lookup Nothing is generic unavailable information;
  GHC.lookupName discards original diagnostics. Preserve checked-local/alias Ids.
- Keyed candidates: canonicalize then owner/occurrence/coarse namespace filter
  BEFORE required lookup. Preserve exactThingMatches as postlookup filtering.
  Empty selection and ordinary value-as-field remain Unknown. Do not mechanically
  remove catMaybes used for legitimate filtering.
- Assert exact Finder/HPT test preconditions. Real uncompiled home, hidden
  package, valid empty exports; labeled UnitState/HPT faults for other branches.
  Broken browse retains mi_exports AND md_exports with empty md_types. Cover
  advertised broken name, truly missing name, unrelated same-spelled broken
  owner, value-as-field and checked current module with no HMI.

## Q10: joined operation attribution

Own exomonad/host/src/actor_host/{operation_identity_oracle,
ordinary_demand_history_tests}.rs only.

- Delete obsolete split/snapshot/JIT oracle and exclusive fixtures.
- Shared private-field PhysicalRequestKey, LogicalCompileRequest and map parser:
  64-hex daemon epoch, positive admission/ordinal, 16-hex logical request;
  reject duplicate physical keys, allow distinct attempts sharing logical id.
- Include fields.execution, span.execution and spans[*].execution consistently.
- Later second cell's Store value 73 joins dispatch -> execution -> Committed
  terminal (compile_count > 0) -> nonempty endpoint submissions. Compare exact
  physical-to-logical maps with host observation and operation-only daemon work.
- Validate unrelated companion work independently; preserve independent daemon
  census and lifecycle checks. Host-only dispatch <= submission <= terminal;
  daemon timestamps are neither host-clock evidence nor END acknowledgement.
- Negative controls: missing dispatch, foreign terminal, missing submissions,
  duplicate key, equal-size identity mismatch, retry logical-id sharing and
  host timestamps outside window. Run actual ordinary-demand history consumer.

## Q6: guarded compilation ownership

Own CompilerRequest.hs, SessionArtifacts.hs, app/Main.hs,
CompilerRequestTest.hs, SourceBootCases.hs and direct consumers. RequestInputsTest
obsolete import removal is coordinated through G2 Haskell.

- Delete compileBoundRequest/raw CheckedEnvironmentResult and preparedCompilationResult
  exits. Concrete checked operations return opaque CheckedCompilation. Prepared
  compilation accepts closed PreparedCompilationTarget (STG or product targets),
  not arbitrary PipelineSelection callbacks. Raw callbacks stay private.
- Checked/PreparedCompilation retain issuing BoundCompilerRequest and one Unique
  per successful compilation, distinct even within one request/executor.
- Opaque PreparedCompilationInterfaces retains completion, output directory and
  raw OriginalInterfaceArtifacts; no unwrap. PreparedCompilationBindings retains
  completion and immutable prepared bindings. ProductContext retains compilation
  identity. Concrete consumers derive the issuer; no independent completion arg.
- Precompute fixities in SessionArtifacts; no retained PipelineResult/HscEnv in
  PreparedSessionBindings. Explicit environment passes only inside guarded owner.
- Guard liveness, identity and admitted native/portable intent BEFORE compiler
  effects or writes, including cached/empty paths. Later certificate validation
  occurs after some current writes and is insufficient for no-write refusal.
- Keep immutable warnings/count/root projections and repeated valid projections.
  Do not purge legitimate internal ExactHydration or GHC APIs.
- Tests: active, closed scope/executor, foreign thread, cancel, cache/empty,
  cross-request and two compilations in one request. A interfaces + B context
  refuses without output changes; repeated A projections succeed; interface
  sealing and session writing after release refuse. No raw test accessor.

## Integration and acceptance

Compile each changed target and affected tests early. Review and integrate G2
Rust/Haskell first; execute all five actor_notebook_contract_tests immediately,
without waiting for Q6. Rerun the three accepted capture-history suites. Fold
Q9/Q10/Q6 as ready and verify joined consumers on the exact consolidated revision.
Argon receives the concrete candidate for adversarial review (purge, failure
paths, meaningful controls), with honest unexecuted boundaries.

Continue remaining g2-completion.md ordinary workflow/lifecycle gates, then one
matched frozen bundle, named Persona subset, spec/tools/rendered-memory checks
and bounded fresh DeepSeek smoke. Retain first new failure inputs/frontier;
never weaken assertions or silently broaden this work. Full G3 restore/fork/
force/recovery/performance matrices and restore deduplication remain separate.

## Live state

- Plan recorded; implementation lanes being dispatched. No new checks executed.
