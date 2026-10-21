# Compiler work and authority map

This map describes source at `5d501a98b9f2f0fc2cd9795e6b3c1e4d5df1558c`
(2026-10-21), on `integration/compiler-query-warm-final`. It separates
implemented mechanisms from the remaining G2 boundaries. Source inspection
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
| Captured output → executable selection | [World native-demand resolver](../tidepool/toolchain/src/compiler_world/native_demand.rs) retains initial request inputs, inventory operation, shared acquisitions and stage batches of `ResolvedNativeDemand`. [WorldProgramContinuation](../tidepool/toolchain/src/declaration_context.rs) retains that request alongside advancing source/TYPE inputs and the actual preceding native product. | `issue_segment_originals` calls `WorldProgramContinuation::stage_inputs`, which checks the actual request/stage/inventory owner and carries its selected groups. Advancing prefix inputs supply SOURCE/TYPE support; their empty `ReceiptNativeInputDemand` does not reconstruct executable demand. `OriginalBindingSelection::from_world_stage` preserves executable selection separately from nominal references. `finish_cell_program_admitted` separately requires matching transaction origin, Clean/NotStarted closure and host cancellation authority before sealing. Retired/Unconfirmed closure refuses. Available bytes or metadata membership do not select a body. |
| Sealed selection → runtime installation | [NativeInstantiationPlan](../tidepool/runtime/src/session/prepared/closed.rs) retains ordered exact imports and each HOME row/package binding paired with its compiled image. `ClosedInstantiationPlan` restricts that same plan to closed code. [PersistentSession](../tidepool/runtime/src/session/persistent.rs) resolves scoped native domains, retained bindings and captured handles. | `admit_turn_code_in` in [resident.rs](../tidepool/runtime/src/session/resident.rs) admits the plan. Captured/open installation and the closed adapter reach `PreparedEngine::install_native_turn`; the old `code_exports` installer is removed. [BindingTable](../tidepool/codegen/src/binding_table.rs) owns scoped instances. Execution requires checked current source facts and retained-capture custody. Sealed Ordinary code restricts the same plan to closed code and captures the actual installing transaction origin; missing Ordinary issuance refuses before preparation. |
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

## Remaining boundaries and routes

- **Target selection:** `AuthenticatedTargetImports::issue` in
  [certified_products.rs](../tidepool/toolchain/src/certified_products.rs)
  validates ordered observations against the actual shared prepared target.
  `ArtifactInventory::select_native_target` in
  [artifact_inventory.rs](../tidepool/toolchain/src/artifact_inventory.rs)
  selects existing exact occurrences from a completed physical cohort and
  retains each ordered import with its exact link. `with_certified_groups`
  checks those slots against issued rows rather than choosing logical matches.
  Finite targets and `admit_native_target_products` in
  [artifacts.rs](../tidepool/toolchain/src/artifacts.rs) consume that owner;
  typed entries must belong to the actual target. Target selection cannot issue
  a new group or replace sealed dependency edges. Nominal metadata remains
  separate from executable occurrence selection.
- **Source authority:** `admit_turn_code_with_images_in` in
  [resident.rs](../tidepool/runtime/src/session/resident.rs) refuses Ordinary
  code without its issued original, then checks that the sealed original
  matches the actual prepared bundle. Ordinary, activation-preview and host
  prototype installation restrict the existing native plan and capture the
  installing source transaction's origin; prototypes with request sites refuse.
  Execution instead requires `current_native_sources` plus validated retained
  source custody in
  [persistent.rs](../tidepool/runtime/src/session/persistent.rs).
  `NativeInstallationSources::current` now requires those facts rather than
  filling absent current facts with Unknown. This repairs the former handoff
  gap in canonical source. Scoped Ordinary effect and captured-source cutoff
  passes belong to their recorded predecessor revision in the ledger;
  they do not qualify the finite → public World journey or full launch.
- **TYPE and lexical support:** `readWorldImportBasis` in
  [GhcPipeline.hs](../bridge/haskell/src/Tidepool/GhcPipeline.hs) retains exact
  carried TYPE entries separately from lexical roots/edges. Requirement-only
  rows hydrate compiler details without becoming lexical imports or native
  grants. Prior-stage native products retain their issued source contribution;
  completing a later target must preserve the same occurrence, not reconstruct
  it from an owner name. The matched mixed preparation/sealing and support laws
  now pass at the ledger's exact compiler checkpoint. These are compiler gates,
  not runtime publication or recovery qualification. Original/Canonical custody
  and Joined/Value pinned external-root observations remain distinct.
- **Accumulated completion work:** `NativeSelectionCompletions::append` in
  [compiler_projection.rs](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs)
  indexes completion owners and exact HOME row conflicts. Incoming rows still
  require checks; `with_native_completion_refs` and `refreshed_native_facts`
  still fold accumulated custody. No end-to-end timing saving is established.
- **Intentional finite bootstrap and test surface:**
  `agent_spec/preparation.rs::compile_installer` uses a retained prepared entry
  when available, otherwise the explicit developer/fixture
  `compile_unprepared_installer` → `run_turn` path. The ordinary actor
  `prepare_checked_host_cell` requires `CellProgramBasis::World`; it has no
  finite fallback after a failed World cell. Finite checked-cell admission
  remains exercised by `checked_quoter_program` and exact-recovery tests.
- **Remaining production binding/retraction route:**
  `resident.rs::bind_prepared`, reached by nonchecked completion and
  `mount_host_handle_prepared`, can call
  `persistent.rs::bind_replacing_decls_in` → `retract_value_heads_in` →
  `SessionLib::prepare_retraction_in`. An attached generation candidate then
  reaches declaration validation and `run_inspections_strict` in
  [session/mod.rs](../tidepool/runtime/src/session/mod.rs). This is a concrete
  remaining finite mechanism. Direct `run_admitted_inspections` callers are
  tests; their presence does not establish a second ordinary query route.

## Receiving G2 work

The [G2 contract](../plans/shared-warm-compiler/completion/g2-completion.md)
owns assignment and acceptance. The accepted Plan B extends these existing
owners: one physical issuer, ordered exact target selectors, nominal metadata
without execution permission, and actual source facts for each installation.
It does not add another installation framework. Per-install source, exact-target
and prior-stage consumer cutovers are integrated. Compiler preparation/sealing has bounded joined evidence; runtime
selection transfer and the full ordinary journey remain open.

| Boundary | Remaining acceptance |
|---|---|
| Compiler boundary qualification | Preserve the matched target, carried TYPE, prefix and support/refusal evidence. The original-package projection negative uses an invalid broad package-row premise and is under fixture repair; it is not a passing refusal control or a confirmed production defect. |
| Runtime selection/publication | Scoped Ordinary effect/cutoff controls pass at their recorded revision. The finite journey reaches 42/display/publication but loses selected package-root domains despite unchanged exact package bindings; transactional selection transfer is under repair. This remains an executed failure, not a runtime milestone pass. |
| Native build and custody qualification | Native actions require `bind_owned` and declared compiler-service resources; portable actions remain direct. Qualify actual catalog production and the exact frozen bundle through [package qualification](../build/package/qualification.py). Retain exact-source limits on custody/recovery passes. |
| Joined ordinary workflow | Execute finite source → installer → public World → repeated package-using cells, A retirement and new B execution with genuine captured-A refusal. Qualify discovery, private inputs, reload and caller loss before selected Persona startup and real DeepSeek smoke on one bundle. |

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
