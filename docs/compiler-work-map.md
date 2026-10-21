# Compiler work and authority map

This map describes source at `d7f92c2b27b33d691cc07916d74c74d38484afb1`
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
| World → captured compiler output | Haskell [CompilerRequest](../bridge/haskell/src/Tidepool/CompilerRequest.hs), [CompilerProducts](../bridge/haskell/src/Tidepool/CompilerProducts.hs) and [FinalizedModuleArtifacts](../bridge/haskell/src/Tidepool/FinalizedModuleArtifacts.hs) carry parsed stages, finalized interfaces/Core, input associations and completed products. `WorldCompilerCompletion` couples World inputs to the pending dependency completion from the same request. | Rust [compiler_world.rs](../tidepool/toolchain/src/compiler_world.rs) admits the checked program; `prepare_world_cell_program` in [artifacts.rs](../tidepool/toolchain/src/artifacts.rs) walks its actual stages through `WorldProgramContinuation`. [Certified products](../tidepool/toolchain/src/certified_products.rs) validate physical output and exact owner evidence. A worker reply alone cannot seal executable output. |
| Captured output → executable selection | [World native-demand resolver](../tidepool/toolchain/src/compiler_world/native_demand.rs) retains initial request inputs, inventory operation and stage batches of `ResolvedNativeDemand`. [Declaration context](../tidepool/toolchain/src/declaration_context.rs) and [native certification](../tidepool/toolchain/src/certified_products/native_demand.rs) distinguish exact bindings, available products and executable choices. | Stage certification and `finish_cell_program_admitted` consume issued evidence. The latter requires matching transaction origin, Clean/NotStarted closure and host cancellation authority before sealing. Retired/Unconfirmed closure refuses. Available bytes or metadata membership do not select a body. Stage/prefix composition is still receiving G2 work below. |
| Sealed selection → runtime installation | [PersistentSession::install_certified_turn_in](../tidepool/runtime/src/session/persistent.rs) resolves exact runtime imports and source leases; [prepared.rs](../tidepool/runtime/src/session/prepared.rs) owns native staging/installation. [BindingTable](../tidepool/codegen/src/binding_table.rs) owns scoped instance registration. | `CheckedTurnCompletion::install_certified_turn_in` in [admission.rs](../tidepool/runtime/src/session/admission.rs) checks the actual in-flight item/prefix and refreshes its World association after native installation. Installation creates runtime state; a compiler type or certificate is insufficient. Package lookup still consults machine-wide `code_exports`; the full package-instance cutover is pending. |
| Successful item → successor/publication/recovery | [CellProgram::item_successor / declaration_item_successor](../tidepool/toolchain/src/compiler_world.rs) issue `CheckedWorldSuccessor` from the actual ordered item, predecessor and installed delta. Runtime [admission.rs](../tidepool/runtime/src/session/admission.rs) owns settlement; [persistent.rs](../tidepool/runtime/src/session/persistent.rs) owns public promotion. | `adopt_checked_world_successor_in` replaces `Tip.exact_context` and `Tip.inspection_world` together under source/target admission, then invalidates the cached compile view. Public promotion commits bindings and the prepared World with the manifest claim. [Recovery hydration](../tidepool/runtime/src/session/recovery_hydration.rs) and [exact-context acquisition](../tidepool/toolchain/src/exact_context.rs) restore selected facts; graph observation alone supplies no execution/source authority. Completion propagation and recovery cutovers remain G2 obligations. |

The actor entry points are in
[resident_workbench.rs](../exomonad/actor/src/resident_workbench.rs):
`actor_compile_view` captures the runtime view with source inputs;
`publish_private_execution` submits the completed private execution for
publication. [Owned workbench execution](../exomonad/actor/src/resident_actor/owned_workbench.rs)
keeps that publication task under the actor owner. Public reload enters through
[owned_workbench/reload.rs](../exomonad/actor/src/resident_actor/owned_workbench/reload.rs).
These callers still need joined workflow qualification; finding them is not a
passing reload or discovery result.

## Distinctions to preserve

| Fact | Issuing/retaining owner | What it does not establish |
|---|---|---|
| Exact compiler roles and selected interfaces | [CompilerSourceSupport / CompilerInputProjection](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs) | Lexical visibility, native body demand or live value access merely from TYPE inputs |
| Immutable graph and exact historical binding | [Exact context](../tidepool/toolchain/src/exact_context.rs), [artifact inventory](../tidepool/toolchain/src/artifact_inventory.rs) | Selection of every retained alternative; logical/image equality cannot replace issued binding identity |
| Physical bytes and acquisition lifetime | Artifact views, captured compiler outputs and retained image plans | Compiler roles, executable roots or runtime instance authority |
| Native availability versus demand | `IssuedNativeOffer`, `RequestNativeAvailability`, `ResolvedNativeDemand` in [declaration_context.rs](../tidepool/toolchain/src/declaration_context.rs) | Full permission from partial offers, or execution from an authenticated but unselected dependency |
| Runtime instance and source permission | [BindingTable](../tidepool/codegen/src/binding_table.rs), source/target leases and checked runtime admission | Reuse of another installation's mutable state because immutable code is shared |
| Successful execution versus publication | Checked prefix completion and public manifest claim | Published names after failure/cancellation, or reversal of external effects already executed |

The full runtime-context commitment, narrow compiler-input identity and native
demand are separate facts. `CompilerWorldRestoration` retains runtime blocks as
well as the compiler projection; narrowing Haskell inputs does not remove all
Rust graph work. Observations such as `last_admission_residency` may already be
stale after END. Preserve the issued owners through the next consumer instead
of reconstructing them from module names, generation labels or retained catalogs.

## Remaining routes at this source revision

- **Closed startup:** `PreparedSourceEntry::prepare` in
  [prepared.rs](../tidepool/runtime/src/session/prepared.rs) validates original
  input custody and resolves against an empty lexical environment to retain
  native images. `prepare_source_startup_entry` in
  [resident.rs](../tidepool/runtime/src/session/resident.rs) mints an isolated
  scope, then calls `resolve_closed_source_entry_in` again. The compiled accessor
  is private to the session module; a reusable closed recipe is still pending.
- **Package imports:** `install_certified_turn` in
  [prepared.rs](../tidepool/runtime/src/session/prepared.rs) checks
  `self.code_exports` before target definitions for `ImportOwner::Package`.
  Its comment about avoiding a machine-global mutable-instance cache describes
  source imports, not the whole installer. Package lookup, staging and export
  publication remain actual routes to delete, not completed cutovers.
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
| Native compiler output and stage composition | One exhaustive paired target/package output for every NativeOriginals route; provenance from the actual compiler producer/profile; compose the held initial demand with actual prefix products, retaining latent exact requirements. |
| Completion through reuse and recovery | Carry complete HOME-slot/package associations and exact package graph bindings through input transformations, successful items, source capture and versioned materialization. The current checked successor/atomic tip replacement is a foundation, not proof of the complete journey. |
| Closed source and package instances | Retain one immutable `ClosedInstantiationPlan`; bind fresh state without second resolution. Preserve exact logical binding plus `SourceInstanceDomain` through the existing BindingTable, rollback, capture and retirement. Delete `code_exports`/RetainedPackage fallbacks and obsolete wire/recovery variants. |
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
