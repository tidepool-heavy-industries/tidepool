# Compiler work and authority map

This map was checked against joined source `f5e8cb121a` (2026-10-22), with the
checked-value instance-scope boundary updated for `c92fb6217b`, and publication,
TYPE and exact restoration boundaries updated through `ccc5dedfe2`.
The linked-code, physical-stage, acquired-prefix, source-scope and Context9
consumer tranche is joined in source; G2 remains unqualified. Source inspection
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
| Source imports → completed import admission | [ImportAdmission](../bridge/haskell/src/Tidepool/ImportAdmission.hs) exposes opaque completed owners; [GhcPipeline](../bridge/haskell/src/Tidepool/GhcPipeline.hs) owns qualification, selected-owner resolution, actual frontend and completion transitions through the private representation. Captured source and scoped request facts precede the actual root's matched driver syntax. | Early matched syntax is DATA for planning and source qualification. After `cpBeforeModule` selectively installs source-less interfaces, preparation resolves exact owners and refines its own parsed module. Successful whole compilation issues the compact completed owner; a parsed or typechecked module alone cannot issue the terminal receipt. |
| World/finite request → captured compiler output | Haskell [CompilerRequest](../bridge/haskell/src/Tidepool/CompilerRequest.hs) acquires `PreparedOutputAuthority` before compilation and returns `NativeOutputs` with its product context or explicit `PortableOutputs`. [CompilerProducts](../bridge/haskell/src/Tidepool/CompilerProducts.hs) retains package bodies and captured defining interfaces under the actual producer/profile; World completion also carries the same request's pending dependencies. | Rust [invocation_output.rs](../tidepool/toolchain/src/artifacts/invocation_output.rs) owns named output layouts and `CapturedNativeOutput`, whose private World/Finite variants retain bytes, provenance and inventory operation. Finite native extraction requires an admitted package-service profile; portable output cannot issue native products. [Certified products](../tidepool/toolchain/src/certified_products.rs) authenticate the captured bytes. A reply alone cannot seal executable output. |
| Captured output → physical cohort and target selection | [CapturedCompilerStage](../tidepool/toolchain/src/declaration_context.rs) owns the actual operation, captured facts, demanded-row issuer and whole-stage output. `CertifiedProducts` privately pairs that stage with its advancing prefix and authoritative selection. [World native-demand resolver](../tidepool/toolchain/src/compiler_world/native_demand.rs) retains initial request inputs and stage demand; `WorldProgramContinuation` advances source/TYPE inputs separately. | `PhysicalCohortInputs` borrows the issued bundle; `ArtifactInventory::select_native_target` consumes it with the actual ordered prepared-target imports. Item prefixes cannot issue physical capture. `stage_inputs` checks the request/stage/inventory owner; an empty prefix `ReceiptNativeInputDemand` does not reconstruct executable demand. `OriginalBindingSelection::from_world_stage` keeps nominal references separate. `CompiledArtifacts`, compile-input sealing and declaration joins carry `IssuedOriginalOutput` instead of reconstructing selection/view pairs. |
| Selected target → linked code and acquisition | [NativeCodeComponent / NativeCodeRef](../tidepool/repr/src/native_code.rs) own immutable SCCs, canonical bodies and ordered edges. [LinkedNativeTarget / AcquiredNativeClosure](../tidepool/toolchain/src/linked_native.rs) pair the actual target with exact HOME grants, PACKAGE receipt occurrences and retained requirements. [link_native_target](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs) is the demand producer; unused availability does not become a code root. | HOME grants retain their certified scoped row; PACKAGE grants retain authenticated products and reached receipt/child occurrence choices. Pure `CodeId` equality permits immutable sharing, not acquisition or Current authority. `physical_current_for` checks the actual target, acquired closure and receiving source-map owner before lending Current. Restored prefix handles are historical. The old completion-row and executable package-scope graphs are removed. |
| Linked target → runtime installation | [NativeInstantiationPlan](../tidepool/runtime/src/session/prepared/closed.rs) is a compiled-image projection of the same `LinkedNativeTarget`; it does not reconstruct HOME/PACKAGE topology. `ClosedInstantiationPlan` restricts that projection to code without retained imports. [PreparedMachine native slots](../tidepool/codegen/src/prepared_program/machine/native_slots.rs) reuse dependency instances by live source domain and closed `CodeId`; invocation targets stay fresh. Reuse keeps the original dependency `ProgramFacts`, prepared code owner and source custody. | `admit_turn_code_in` in [resident.rs](../tidepool/runtime/src/session/resident.rs) and `PreparedEngine::install_native_turn` retain the existing transaction/permit boundaries. Each actual acquired occurrence is source-authorized before code/domain reuse; historical absence cannot borrow Current from an equal-code sibling. [BindingTable](../tidepool/codegen/src/binding_table.rs) owns scoped instances. Retained captures still require actual type/generation/live custody; parcels preserve existing instances through their own route. Missing Ordinary issuance refuses before preparation. |
| Successful item → successor/publication/recovery | [CellProgram::item_successor / declaration_item_successor](../tidepool/toolchain/src/compiler_world.rs) issue `CheckedWorldSuccessor` from the actual ordered item, predecessor and installed delta. [CompilerSourceSupport](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs) carries actual acquired closure owners through input transformations; these are compiler proofs, not an installed-state graph. Runtime [admission.rs](../tidepool/runtime/src/session/admission.rs) owns settlement; [persistent.rs](../tidepool/runtime/src/session/persistent.rs) owns public promotion. | `adopt_checked_world_successor_in` replaces `Tip.exact_context` and `Tip.inspection_world` together under source/target admission, then invalidates the cached compile view. Public promotion commits bindings and the prepared World with the manifest claim. [Exact-context writer/reader](../tidepool/toolchain/src/exact_context.rs) requires version 9 with independent source-scope facts, shared linked components and exact acquired occurrence records. [Recovery hydration](../tidepool/runtime/src/session/recovery_hydration.rs) restores selected facts; graph observation supplies no execution/source authority. The integrated structures still need joined workflow qualification. |

`PreparedOutputAuthority` obtains native provenance from the active
`CompilerScope`'s process producer and validated service-package association;
missing configuration refuses. [CanonicalProducerIdentity](../tidepool/toolchain/src/artifact_inventory.rs)
is SHA-256 of stable producer bytes, distinct from endpoint identity. The current
matched readers require TPCERT13, TPHOMEOWNERS6, World source receipt TPCWSOURCE4 and
[worker request v29](../tidepool/extract-cmd/src/request.rs). These formats and
the version-9 context cutover are implemented source contracts, not pending
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
the private artifact descriptor. Captured-product admission and explicit acquired
payload custody reuse it instead of rebuilding it. Executable `CodeId` derives
from canonical bodies and closed ordered dependencies, not an acquisition cohort.
This trades retained
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
| Authored source permission versus original instance adjacency | [SourceLexicalScope / ExactDeclarationContext](../tidepool/toolchain/src/declaration_context.rs) | Source permission from compiler TYPE closure; complete display instance evidence merely from an admitted native entry |
| Immutable graph and exact historical binding | [Exact context](../tidepool/toolchain/src/exact_context.rs), [artifact inventory](../tidepool/toolchain/src/artifact_inventory.rs) | Selection of every retained alternative; logical/image equality cannot replace issued binding identity |
| Physical bytes and acquisition lifetime | Artifact views, captured compiler outputs and retained image plans | Compiler roles, executable roots or runtime instance authority |
| Joined interface materialization | [CertifiedJoinedInterface::materialize](../tidepool/toolchain/src/recovery_artifacts.rs) consumes its owned interface/sidecar bytes; raw path-based Joined materialization APIs have been removed | Capture of every external package root named by that sidecar, or native/source authority from interface bytes |
| Native availability versus demand | `IssuedNativeOffer`, `RequestNativeAvailability`, `ResolvedNativeDemand` in [declaration_context.rs](../tidepool/toolchain/src/declaration_context.rs) | Full permission from partial offers, or execution from an authenticated but unselected dependency |
| Linked code versus acquired permission | [NativeCodeComponent](../tidepool/repr/src/native_code.rs) describes immutable code; [AcquiredNativeCode / LinkedNativeTarget](../tidepool/toolchain/src/linked_native.rs) carry actual scoped grants and ordered receipt choices | Acquisition, Current source membership or mutable instance identity from pure `CodeId`; nominal metadata and PACKAGE payload custody do not add compiler roles or executable roots |
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
  typed entries must belong to the actual target. `CheckedTypedEntry` authenticates
  the generated owner, exact Original member and prepared entry. Target admission
  and `PreparedCheckedItemOutput::capture` consume its same-target proof alongside
  the plan, root, item and receipt checks. Native entry capture does not require a
  complete original instance environment; display and source publication retain
  that separate requirement. Target selection cannot issue a new group or replace
  sealed dependency edges. Nominal metadata remains separate from executable
  occurrence selection.
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
- **Import admission:** generated roots carry immutable captured bytes, path and
  producer-associated import origins; ordinary source derives Authored imports
  from the actual driver, preserving admitted CPP. `ScopedImportAdmission` binds
  the admitted finite scope or World identities to the request, incarnation and
  full root. Its private `MatchedImportDriver` retains syntax correspondence and
  current-source selection DATA before actual owner resolution.
  [GhcPipeline.hs](../bridge/haskell/src/Tidepool/GhcPipeline.hs) keeps the existing
  source selectors, canonical validators and finite/World hydration policies.
  It acquires the actual downsweep root parse once, then reuses that owner for
  quotation planning, current-source qualification and the three frontend paths.
  `parseModuleWithImportRoot` guards the full Module, source kind, canonical path,
  source fingerprint and effective language/extensions/safety/import-path policy;
  an hs-boot summary remains a separate parse. After `cpBeforeModule` installs
  precisely demanded source-less interfaces, `prepareImportAdmission` resolves
  selected Modules and carries the existing `CheckedValueImportAuthority` into
  `PreparedImportAdmission`. A selected checked import must have that full Module
  installed in the HPT and cannot select a boot interface. The closed refiner
  consumes the same owned driver. Narrowed
  and qualified descendants must preserve their parent's selected owner and
  import restriction. Frontend summary rebinding preserves the parsed body and
  occurrence lineage; callers cannot pair a replacement AST with that proof.
  Prepared evidence is recorded only after the actual owned frontend succeeds.
  Checked completion additionally requires its unique successful target and
  candidate revalidation; PreparedStg/PreparedProducts completion follows successful
  executable compilation and the relevant final checks. `ExactCompilation`
  structurally owns finite completion, while World inputs retain their correlated
  World completion. Terminal projections retain compact consumed decisions and
  perform no Finder resolution; the successful parsed owner is released from the
  attempt reference. Completion keeps checked imports outside canonical SOURCE
  roles, refuses any checked/consumed-SOURCE overlap and requires exact equality
  between selected nonchecked HOME imports and the consumed SOURCE census. SOURCE
  wire evidence and `exactImports` are unchanged; checked TYPE custody retains its
  independent issuer. The detached import-intent parser/selection route is removed.
  This boundary is source-reviewed, with checked-value and carried-World TYPE
  controls executed at their recorded revisions. Broader ordinary and runtime
  histories remain execution obligations. Relevant
  controls include `moduleHeaderCompilation`, `generatedScaffoldImports`,
  `checkedCommandValue` and `worldCarriedImportTypes`; migrated callers and linked
  test executables alone do not establish their passing behavior.
- **TYPE and lexical support:** `readWorldImportBasis` in
  [GhcPipeline.hs](../bridge/haskell/src/Tidepool/GhcPipeline.hs) retains exact
  carried TYPE entries separately from lexical roots/edges. In
  [ExactScope.hs](../bridge/haskell/src/Tidepool/ExactScope.hs),
  `readWorldTypeInputs` follows only exact Context-selected requirement edges and
  partitions Original and `CheckedValueEvidence` rows for the existing checked
  interface decoder. Checked values keep their authenticated role and owner;
  hydration adds neither visible bindings, authored source permission nor native
  grants. `readWorldImportBasis` retains its stricter lexical admission checks.
  Prior-stage native products retain their issued source contribution; completing
  a later target preserves the same occurrence instead of reconstructing it from
  an owner name. Original/Canonical custody and Joined/Value pinned external-root
  observations remain distinct. Predecessor TYPE/support passes do not qualify
  this joined closure repair or runtime publication and recovery.
  Haskell's private `WorldCellPrefix` keeps actual HMI objects and the recursive
  home-interface usage closure required by successful Originals, including
  staged value interfaces when genuinely used. `restoreWorldCellPrefixHomes`
  restores those HPT/Finder owners separately from the justified lexical
  `ModuleGraph`; an interface dependency does not become a lexical import.
  `advanceWorldCellPrefix` maps captures to slots by typed `SessionModule`
  (kind plus generation), so `Lib.Gn` and `Val.Gn` cannot collide by generation
  alone. Captured values must be `ValMod` in the current item window. Repeated
  observations preserve the already-held compatible HMI owner before considering
  a fresh finalized observation; this source policy has no independent execution
  claim here.
- **Source-scope separation:** [SourceLexicalScope](../tidepool/toolchain/src/declaration_context.rs)
  carries admitted source permission independently of the broader original
  compiler/instance adjacency. `OriginalInstanceEnvironment` separately records
  whether the original compiler graph is complete enough for display and source
  publication. Ordinary output obtains source scope through actual completed
  source admission and matching canonical output; exact output consumes its
  checked source admission. Generated import-role receipts preserve authored,
  inherited and protected occurrences separately from exact import authorization.
  TYPE-only support extensions add no source scope. Value and source publication
  project admitted roots through their own source closure; retained unrelated
  compiler facts do not become public roots. Both World and finite publication
  retain a checked value's nominal dependencies as TYPE artifact edges without
  adding that value to the accumulated lexical graph. Original-output admission
  issues an instance-empty leaf only for this preparation's actual checked-value
  authority and matching selected thin interface. SOURCE receipt membership is
  neither required nor sufficient for that authority; missing or mismatched
  selected interfaces refuse. Thin interfaces contain no instances, while real
  canonical source adjacency and missing-owner refusals remain unchanged.
  Same-cell prior-stage values have a separate issuer:
  `ProducedValueTypeInterfaces` and exact prior-produced TYPE anchoring. They are
  not rows in `CheckedValueImportAuthority`, which covers admitted base-World
  values and the operation operand. Observe their selected compiler roles, not
  a presumed checked-value instance leaf or mere artifact custody. The
  [SourceScope/Context9 contract](../plans/shared-warm-compiler/completion/linked-code-ownership/source-scope.md)
  is implemented in the joined source, including separate snapshot facts and
  restore validation against actual selected support. The selected facade import
  route requires its actual selected interface plus immutable adjacency; it does
  not grant fresh SOURCE authority. Context9 restores native offers and acquired
  closures against the held exact selection and attaches the authenticated owners
  without repeating live custody-growth composition. Baseline rebinding follows
  the same rule: exact seeds and proof scopes must remain equal. At `ccc5dedfe2`,
  cold attachment passes in the staged runtime history; subsequent World bootstrap
  still refuses ambiguous native ownership. The final cold consumer is unqualified.
- **Acquired-prefix work:** `AcquiredNativeClosures::append` in
  [compiler_projection.rs](../tidepool/toolchain/src/artifact_inventory/compiler_projection.rs)
  retains actual immutable owners, shared component/closure records and a
  payload-only PACKAGE custody index. Repeated actual owners are idempotent;
  equal scalar records do not substitute live grants. `group_owner` lends one
  intact held witness for a new exact HOME demand only when product issuance
  and closed code agree; an already owned witness is not reselected. The old
  global completion-row conflict index is removed. A separate smaller-target
  custody follow-up remains unqualified; selected code does not prove minimal
  physical retention. No end-to-end timing saving is established.
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
owns acceptance; [linked-code ownership](../plans/shared-warm-compiler/completion/linked-code-ownership/README.md)
owns the current implementation tranche. One physical issuer, an advancing
compiler prefix, one immutable code graph and actual acquired/source owners feed
the existing installer. This supersedes the old completion-scope repair plan.
Implementation and focused predecessor results do not qualify the current
ordinary journey, snapshot recovery or frozen bundle.
The [consolidated review checkpoint](../plans/shared-warm-compiler/completion/linked-code-ownership/review-checkpoint.md)
records the current `c92fb6217b` run: preparation and callback assertions clear,
but the initial runtime history refuses duplicate lexical owner Lib.G1. The
component fixture and compiler-test setup failures are recorded separately.

| Boundary | Remaining acceptance |
|---|---|
| Compiler boundary qualification | At `0197741519`, affected production/cfg compilation passes. The [checked-value type-closure leaf](/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/linked-0197-checked-value-one.log) executes 1/1 and reaches all four explicit/inferred and normal/delayed branches, complete TYPE closure, owner refusals and the SOURCE/TYPE distinction. Nested actual-candidate 1/1 and codec 4/4 passes retain their separate scope. The [Context9 control](/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/linked-1d5c-context-nine-one.log) passes 1/1 at `1d5c7d86f8`, covering omitted fields, missing roots, restore and unchanged reuse. Broader snapshot histories remain unqualified; earlier target/TYPE/prefix/package/support results belong to their recorded predecessor sources. |
| Runtime selection/publication | Current runtime production compilation is not an executed runtime history. Staged World, runtime recovery from acquired-owner/Context9 snapshots, Text/Display, acknowledgement and ordinary publication remain unqualified. The native target-proof and checked-import completion boundaries are source-reviewed; their component results do not qualify the ordinary journey. V's warm lane owns exact source pins and terminal reports. |
| World expression reservation | The held World issues collision-free expression and `_unit` names from its actual lexical scope. Rust validates ordered expression coverage and reserves captures from that witness; the actor uses the ordinary World observation path. Missing witnesses still refuse. Joined bare/effectful expression execution and publication remain to qualify through semantic histories. |
| Native build and custody qualification | Native actions require `bind_owned` and declared compiler-service resources; portable actions remain direct. Qualify actual catalog production and the exact frozen bundle through [package qualification](../build/package/qualification.py). Retain exact-source limits on custody/recovery passes. |
| Joined ordinary workflow | Execute finite source → installer → public World → repeated package-using cells, A retirement and new B execution with genuine captured-A refusal. Qualify discovery, private inputs, reload and caller loss before selected Persona startup and real DeepSeek smoke on one bundle. |

The [linked-code verification plan](../plans/shared-warm-compiler/completion/linked-code-ownership/verification.md)
names the early staged World controls: toolchain preparation first, then the
small runtime history through declaration prefixes, first package-free and
package-bearing demand, forced Display, real effects and a fresh public consumer.
Their selected-demand observations are separate from immutable carrier availability;
they do not infer graph identity from scoped acquisition IDs. These are acceptance
contracts, not passing evidence for the ownership migration.

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
