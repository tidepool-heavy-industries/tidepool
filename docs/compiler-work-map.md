# Compiler work and authority map

This map describes source at `b856a75d5c7ba344632e1935d3c4c4d5192ea5b8`
(2026-10-20), on `integration/compiler-query-warm-final`. It separates the
implemented path from accepted G2 work still being received. Source inspection
establishes ownership and callers; it does not establish executed acceptance,
release qualification or a performance improvement.

## Current dataflow

Read this sequence from an authored request to its next usable state. The
observation path shares World acquisition and source validation, then returns
through answer settlement rather than native installation.
Native-demand exchange occurs during compilation; these are ownership
boundaries, not disjoint timing phases.

| Boundary | Authoritative owner and carried facts | Consumer and authority check |
|---|---|---|
| Source/request → compiler inputs | Runtime [source admission](../tidepool/runtime/src/session/source_admission.rs), [cell admission](../tidepool/runtime/src/session/admission.rs) and [inspection](../tidepool/runtime/src/session/inspection.rs) retain the actual source, target lease, current-view observations and ordered cell reservation. | `prepare_cell_program_admitted_inner` in [turn.rs](../tidepool/runtime/src/session/turn.rs) checks preparation admission and execution purpose before dispatch. `run_admitted_compiler_world` / `run_admitted_world_scope_inspection` acquire a World or answer under their own admission. A captured view describes state; it grants neither source access nor execution. |
| Inputs → held compiler World | Rust [CompilerWorldRestoration / CompilerWorld](../tidepool/toolchain/src/compiler_world.rs) retain producer/profile, exact context, compiler-input projection, bindings and lexical facts. Haskell [HeldCompilerWorld](../bridge/haskell/src/Tidepool/GhcPipeline.hs) retains admitted input facts, realization, backing and prepared modules. | [Main.hs](../bridge/haskell/app/Main.hs) dispatches exact-input acquisition to `scopedEnsureWorld`. `runHeldWorldSource` serves cell/observation preparation; `runWorldPublicModule` resolves public modules from captured source selection. Realization/residency is an observation of a worker admission, not a lease or runtime permission. |
| World/finite request → captured compiler output | Haskell [CompilerRequest](../bridge/haskell/src/Tidepool/CompilerRequest.hs) acquires `PreparedOutputAuthority` before compilation and returns `NativeOutputs` with its product context or explicit `PortableOutputs`. [CompilerProducts](../bridge/haskell/src/Tidepool/CompilerProducts.hs) retains package bodies and captured defining interfaces under the actual producer/profile; World completion also carries the same request's pending dependencies. | Rust [invocation_output.rs](../tidepool/toolchain/src/artifacts/invocation_output.rs) owns named output layouts and `CapturedNativeOutput`, whose private World/Finite variants retain bytes, provenance and inventory operation. Finite native extraction requires an admitted package-service profile; portable output cannot issue native products. [Certified products](../tidepool/toolchain/src/certified_products.rs) authenticate the captured bytes. A reply alone cannot seal executable output. |
| Captured output → executable selection | [World native-demand resolver](../tidepool/toolchain/src/compiler_world/native_demand.rs) retains initial request inputs, inventory operation, shared acquisitions and stage batches of `ResolvedNativeDemand`. [WorldProgramContinuation](../tidepool/toolchain/src/declaration_context.rs) retains that request alongside advancing source/TYPE inputs and the actual preceding native product. | `issue_segment_originals` still derives demand from receipts; the retained native stage batches have no downstream certification consumer yet. `finish_cell_program_admitted` separately requires matching transaction origin, Clean/NotStarted closure and host cancellation authority before sealing. Retired/Unconfirmed closure refuses. Available bytes or metadata membership do not select a body. |
| Sealed selection → runtime installation | [ClosedInstantiationPlan](../tidepool/runtime/src/session/prepared/closed.rs) retains ordered imports and `ClosedCompiledProduct`: each HOME row or package binding is paired with its own compiled image. [ResidentSession](../tidepool/runtime/src/session/resident.rs) installs that plan for prepared source or a closed checked cell. Captured/open code still uses [PersistentSession::install_certified_turn_in](../tidepool/runtime/src/session/persistent.rs) and scoped resolution. | [BindingTable](../tidepool/codegen/src/binding_table.rs) owns scoped instance registration. Closed plan construction validates the batch; installation reuses it under the actual source origin and domain. Immutable image sharing grants no access to another instance. The older captured/open installer still consults machine-wide `code_exports` for package imports. This is a partial cutover, not a claim about every installation path. |
| Successful item → successor/publication/recovery | [CellProgram::item_successor / declaration_item_successor](../tidepool/toolchain/src/compiler_world.rs) issue `CheckedWorldSuccessor` from the actual ordered item, predecessor and installed delta. [CompilerSourceSupport](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs) carries issued native completions through input transformations. Runtime [admission.rs](../tidepool/runtime/src/session/admission.rs) owns settlement; [persistent.rs](../tidepool/runtime/src/session/persistent.rs) owns public promotion. | `adopt_checked_world_successor_in` replaces `Tip.exact_context` and `Tip.inspection_world` together under source/target admission, then invalidates the cached compile view. Public promotion commits bindings and the prepared World with the manifest claim. [Exact-context writer/reader](../tidepool/toolchain/src/exact_context.rs) requires version 7 with native completion facts. [Recovery hydration](../tidepool/runtime/src/session/recovery_hydration.rs) restores selected facts; graph observation supplies no execution/source authority. The integrated structures still need joined workflow qualification. |

`PreparedOutputAuthority` obtains native provenance from the active
`CompilerScope`'s process producer and validated service-package association;
missing configuration refuses. [CanonicalProducerIdentity](../tidepool/toolchain/src/artifact_inventory.rs)
is SHA-256 of stable producer bytes, distinct from endpoint identity. The current
matched readers require TPCERT11, TPHOMEOWNERS6 and
[worker request v29](../tidepool/extract-cmd/src/request.rs). These formats and
the version-7 context cutover are implemented source contracts, not pending
layout proposals or proof of a matched frozen bundle.

The initial request fixes external native acquisition and its inventory owner.
Each accepted item can advance compiler source/TYPE support and retain its exact
native predecessor without changing that initial owner.
`WorldProgramContinuation::pending_successor` shares immutable predecessor state;
`admit_native_item` prepares source support, optional value TYPE support and native
completion, then replaces the continuation after the final checkpoint. Selected
source-owner support uses a persistent set. Its accounting charges logical new
keys, not all internal persistent-tree allocations or process memory.

[CertifiedPackageCodeProduct](../tidepool/toolchain/src/certified_products/package_code.rs)
retains the authenticated envelope SHA-256 separately from its ordered-import
product identity. Materialization uses that envelope digest; restoration checks
the referenced bytes and reconstructs the same identities. Issuance also retains
the private artifact descriptor. Inventory insertion, package-plan identity and
native completion lookup reuse it instead of rebuilding it. This trades retained
descriptor/owner storage for removed query work; no timing improvement is inferred.

Original/Canonical products now retain exact package-interface byte custody.
[PackageInterfaceCustody](../tidepool/toolchain/src/certified_products/package_code.rs)
captures admitted output bytes or independently verified package selections;
its subset operations preserve exact owner/seal obligations without granting
compiler roles or executable demand. During HOME recovery,
[ParsedHomeCertification](../tidepool/toolchain/src/certified_products.rs) decodes
the certificate once, exposes package seals for custody restoration, then moves
the same witness into verified ownership after authentication. The production
[recovery reader](../tidepool/toolchain/src/recovery_artifacts.rs) consumes that
parsed owner; the former two-decode receiving path is no longer current.

The actor entry points are in
[resident_workbench.rs](../exomonad/actor/src/resident_workbench.rs):
`actor_compile_view` captures the runtime view with source inputs;
`publish_private_execution` submits the completed private execution for
publication. [Owned workbench execution](../exomonad/actor/src/resident_actor/owned_workbench.rs)
keeps that publication task under the actor owner. Public reload enters through
[owned_workbench/reload.rs](../exomonad/actor/src/resident_actor/owned_workbench/reload.rs).
These callers still need joined workflow qualification; finding them is not a
passing reload or discovery result.

Failure evidence follows its actual output owner.
`CompilationOutputOwner::retain_failure` in [artifacts.rs](../tidepool/toolchain/src/artifacts.rs)
distinguishes disposable scratch copying from durable Original/inspection output;
hosted diagnostics use tracing, while build/inspection callers report retained
paths on stderr. Diagnostic-write failure preserves the primary typed error.
[OwnedChildLifecycle](../tidepool/extract-cmd/src/frontend.rs) retains the observed
child result before cleanup and keeps cleanup/publication failures separate.
These reports describe execution and resource settlement independently.

## Distinctions to preserve

| Fact | Issuing/retaining owner | What it does not establish |
|---|---|---|
| Exact compiler roles and selected interfaces | [CompilerSourceSupport / CompilerInputProjection](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs) | Lexical visibility, native body demand or live value access merely from TYPE inputs |
| Immutable graph and exact historical binding | [Exact context](../tidepool/toolchain/src/exact_context.rs), [artifact inventory](../tidepool/toolchain/src/artifact_inventory.rs) | Selection of every retained alternative; logical/image equality cannot replace issued binding identity |
| Physical bytes and acquisition lifetime | Artifact views, captured compiler outputs and retained image plans | Compiler roles, executable roots or runtime instance authority |
| Joined interface materialization | [CertifiedJoinedInterface::materialize](../tidepool/toolchain/src/recovery_artifacts.rs) consumes its owned interface/sidecar bytes; raw path-based Joined materialization APIs have been removed | Capture of every external package root named by that sidecar, or native/source authority from interface bytes |
| Native availability versus demand | `IssuedNativeOffer`, `RequestNativeAvailability`, `ResolvedNativeDemand` in [declaration_context.rs](../tidepool/toolchain/src/declaration_context.rs) | Full permission from partial offers, or execution from an authenticated but unselected dependency |
| Completed native selection | [IssuedNativeSelectionCompletion](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs): HOME rows, exact package graph bindings and retained requirements, separately from Original certificates | Promotion of nominal compiler metadata into executable demand; package code is an explicit artifact kind, not a compiler-namespace owner |
| Runtime instance and source permission | [BindingTable](../tidepool/codegen/src/binding_table.rs), source/target leases and checked runtime admission | Reuse of another installation's mutable state because immutable code is shared |
| Successful execution versus publication | Checked prefix completion and public manifest claim | Published names after failure/cancellation, or reversal of external effects already executed |

The full runtime-context commitment, narrow compiler-input identity and native
demand are separate facts. `CompilerWorldRestoration` retains runtime blocks as
well as the compiler projection; narrowing Haskell inputs does not remove all
Rust graph work. Observations such as `last_admission_residency` may already be
stale after END. Preserve the issued owners through the next consumer instead
of reconstructing them from module names, generation labels or retained catalogs.

## Remaining routes at this source revision

- **Closed versus captured/open code:** `PreparedSourceEntry::prepare` retains
  an `Arc<ClosedInstantiationPlan>`; startup borrows it without another resolution.
  Checked cells try structural closedness and otherwise retain scoped resolution.
  [TurnCode](../tidepool/runtime/src/session/turn.rs) shares its prepared program
  through `Arc`; optional certification/output evidence and the captured/open
  route remain distinct from the closed installer.
- **Receipt reconstruction:** `issue_segment_originals` in
  [declaration_context.rs](../tidepool/toolchain/src/declaration_context.rs) still
  calls `ReceiptNativeInputDemand::from_products` and `demand_inputs` against
  advancing prefix inputs. Initial acquisition batches are retained separately;
  their existence does not establish that downstream admission reuses them.
- **Accumulated completion work:** `NativeSelectionCompletions::append` in
  [compiler_projection.rs](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs)
  now indexes owners by completion identity and exact HOME row conflicts by
  `NativeGroupBinding`, avoiding the prior scan through retained owners.
  Incoming rows still require checks. `with_native_completion_refs` and
  `refreshed_native_facts` each fold accumulated completion custody; those folds
  remain, and no end-to-end timing saving is established.
- **Captured/open package imports:** `install_certified_turn` in
  [prepared.rs](../tidepool/runtime/src/session/prepared.rs) checks
  `self.code_exports` before target definitions for `ImportOwner::Package`.
  Package lookup, staging and export publication remain on this older route;
  their presence does not describe the closed plan's instance selection.
- **Finite preparation:** `prepare_cell_program_admitted_inner` in
  [turn.rs](../tidepool/runtime/src/session/turn.rs) takes the World branch when
  admission has a World reservation, otherwise uses `ModuleCandidateOffer`.
  The [route disposition ledger](../plans/shared-warm-compiler/completion/tests/verification-ledger.md#remaining-route-disposition)
  tracks remaining finite declaration validation/retraction callers. World
  observation dispatch does not prove repository-wide compiler-route exclusivity.

These are confirmed source routes. Whether each still fails an ordinary joined
history, and its cost on a frozen bundle, requires the corresponding execution
evidence. No new behavioral or timing result is inferred here.

## Receiving G2 work

The [G2 contract](../plans/shared-warm-compiler/completion/g2-completion.md)
owns decisions, assignment and integration order. This table describes the
remaining boundary, rather than treating every part of G2 as absent from source.

| Boundary | Accepted remaining contract |
|---|---|
| World demand consumption | Consume the actual initial acquisition/stage owners together with advancing source/TYPE prefix facts and captured source closure. Preserve latent exact requirements and nominal metadata separately from executable choices; remove receipt-based demand reconstruction. Atomic prefix and persistent predecessor support are already implemented. |
| Runtime cutover | Common installation and once-only issuance remain feature work. Captured/open installation, `code_exports` and RetainedPackage routes still require their own cutover; the implemented closed product/image pair and domain-owned installer do not prove those routes migrated. |
| Custody qualification | Original/Canonical custody and parsed HOME authentication are integrated source; exact joined ordinary-workflow qualification remains open. Joined/Value producers in [SessionArtifacts](../bridge/haskell/src/Tidepool/SessionArtifacts.hs) and [DeclarationJoin](../bridge/haskell/src/Tidepool/DeclarationJoin.hs) observe pinned installed external package roots; their owned interface/sidecar bytes do not imply ownership of every referenced package body. No captured-path defect or complete TYPE/recovery qualification is established for those producers by this map. |
| Native build qualification | Native build actions require `bind_owned` and the declared compiler-service deployment through [package qualification](../build/package/qualification.py); portable actions remain direct. Owner-specific diagnostics and pre-cleanup child observation are implemented. Actual catalog production and exact frozen-bundle qualification remain separate execution gates. |
| Joined ordinary workflow | Follow real finite source → installer → public World → repeated package-using cells; retire A and execute a new B cell while retaining genuine captured-A refusal. Qualify discovery, private inputs, default reload and caller loss, then selected Persona startup and real DeepSeek smoke on one frozen bundle. |

G2 includes ordinary-workflow correctness. The G2 contract assigns the full
force/fork/callback/process-recovery matrices and full campaign M1–M3 to G3;
unsupported operations must refuse explicitly.

## Evidence and measurement

The [verification ledger](../plans/shared-warm-compiler/completion/tests/verification-ledger.md)
owns commands and result dispositions at their recorded revisions. The
[campaign acceptance matrix](../plans/shared-warm-compiler/qualification/ACCEPTANCE.md)
separates semantic, work-counter, performance, frozen-release and deployment
claims. Use [package qualification](../build/package/README.md) for exact bundle
acceptance. Earlier finite-path inventories and checkpoint prose remain in Git;
they do not describe current pending work.
Cold reacquisition and producer removal are separate observations. A removal
control must observe the actual scoped producer root; a synthetic source token's
parent directory cannot establish that the producer's artifacts were removed.
Earlier canonical cold-check results do not qualify that removal boundary.

Historical measurements remain available in the private
[b108 stage table](/srv/swarm/evidence/inanna/overnight-release-20261018/compiler-work-invariant-atlas-joined-v2-b108-actual/STAGE-TABLE.md),
[admission-tail timer review](/srv/swarm/evidence/inanna/overnight-release-20261018/admission-tail-instrumentation-review.md)
and [baseline terminal receipt](/srv/swarm/evidence/inanna/admission-analysis-review-20261018/overnight-20d7-baseline/BASELINE13-TERMINAL.json).
Their figures belong to their original source/workload, not this map's revision.

Follow [compiler profiling](compiler-profiling.md) for matched producers,
workload history, interval attribution and retained evidence. Nested host spans,
worker stages and outer service times overlap; do not add inclusive timings.
Structural deletion, fewer observed operations and measured latency improvement
are separate conclusions. Use [property testing](property-testing.md) for
independent ownership, reuse and recovery histories.
