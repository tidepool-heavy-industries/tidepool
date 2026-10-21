# G2: complete the native compiler/runtime join

Accepted implementation contract, revised 2026-10-20 after joined architecture
review, consolidated at `6bd1275d4a45f78bb11dbdf17e926af4a75948c4`. Controlling branch:
`integration/compiler-query-warm-final`.

This document owns the remaining G2 sequencing and boundary decisions. It
refines [ownership-completion.md](ownership-completion.md) and
[package-runtime-join.md](package-runtime-join.md). Earlier implementation
shapes may be replaced. No second compiler route, package registry, test
framework, or fallback is authorized by preserving unfinished scaffolding.

## Acceptance

One exact frozen bundle supports ordinary Persona startup, notebook history,
discovery, retained values and default reload. Verify the selected Persona spec,
tools and initial memory, then run a bounded real DeepSeek multi-turn smoke.
Start fresh conversation state while preserving authored files and memory.

G2 cannot defer a known ordinary-workflow failure, uncertain-END execution,
wrong historical links, accidental A-state reuse or caller-loss corruption of
published B. Full force/fork/callback/process-recovery matrices and full M1–M3
belong to G3. Unsupported G3 operations must refuse explicitly.

## Current implementation: Plan B

Complete these existing owners together; this section governs the current
implementation split. The detailed invariants below remain constraints, not a
request to rebuild completed infrastructure.

### Ordered target admission

The physical cohort issues native availability once. Authenticate the actual
`Arc<PreparedProgram>` and its ordered globals, then select already-issued exact
occurrences. Authentication checks full symbol identity, representation,
structural signature, evaluatedness and generation. It grants no execution.

Evolve `IssuedTargetNativeSelection` to carry ordered exact slots and an explicit
entry occurrence. Derive roots from those slots. Current Source uses its own
completed physical receipt after the actual current-stage gate; Original uses
its exact chosen binding and inseparable acquisition/contribution. Preserve the
current source offer's solved dependency causes and historical issued edges.
Delete target-side group issuance, broad parent indexing, and logical
`for_ordinary_target` / `with_certified_groups` / `resolve` rematches. Metadata
custody, package preparation, image readiness and runtime source admission remain
separate stages.

### Nominal metadata and prior produced Source

Hash and compare retained core using authenticated nominal imports. Do not
resolve unused Original imports as executable demand merely to obtain identity.
The private enum keeps full Source owners, exact Original interface/body pairs,
Retained identities/generations and Package metadata exhaustive. Preserve the
existing Haskell `original-binding` encoding, `retained-core-home-v1` and HOME6;
no format fork or normalization back into Source. Sharing still checks the actual
body, census and witness. Accidental old ArtifactId equality is not a contract.

Enrich the existing `ProgramSourceSupport` owner row with the actual sealed
stage's `Arc<CompletedNativeProductAvailability>`. Carry that same row into
compiler inputs and facts. Later explicit Source demand selects its exact issued
occurrence; it cannot promote mere metadata, initial-empty selection, or a
Partial offer. `CertifiedSegmentOriginals::physical_inputs()` owns the physical
cohort. The post-`select_item` compiler prefix is a different owner and cannot
substitute for it.

### Captured TYPE imports

The Haskell World source-input owner holds a private demand-selected
`WorldImportBasis`. Parsed imports and existing `ImportIntent` distinguish
current authored source from carried generated imports. Hydrate the reached TYPE
closure with existing ExactHydration before downsweep/rename, and authorize its
actual lexical instance/family graph. The same interface owner supplies GHC
Names and exact `WorldOriginalReference` pairs. HPT population alone is not the
complete migration.

Source-less carried roots require actual canonical SourceOriginal lexical
adjacency. TYPE requirements are not automatically lexical edges. Preserve
existing generated declaration/value authorities without broad HPT exemptions.
No source resurrection, whole-inventory scan, native grant from TYPE or retained
mutable HscEnv. Public-module queries retain their existing provenance contract;
a broader query variant needs a demonstrated separate requirement.

### Source-owned installation

Reuse `NativeTurnAdmission::{Closed, Native, Portable}`. A closed restriction
borrows the same `Arc<NativeInstantiationPlan>` and images, returning a typed
refusal when open. It is not a second planner or optional fallback.

Sealed Ordinary startup and `run_with_sites` carry their actual installing
SourceTransaction through the existing scope. Checked execution requires actual
CurrentNativeSources: remove the optional-owner-to-Unknown path. Historical and
reused instances retain their ProgramFacts. Unsupported open Ordinary/prototype
shapes refuse explicitly; preview retains its affine mounted-source check.
A shared production resolve/install path is appropriate where it preserves those
same owners and pre-allocation admission.

### Qualification order

1. Compile joined production and cfg(test) consumers. Run a genuine imported HOME
   target through installation and an actual mixed World history first.
2. Run finite source → installer I_B → public B → repeated cells, including real
   retained functions, suspension and source custody. Pure Int proves no effect
   source attribution. Keep same-issuance aliases separate from same-payload
   distinct-binding target controls.
3. Execute ordinary syntax/import/private-input/discovery/Ready/reload controls,
   caller loss, cancellation, uncertain END and required historical cold/eviction
   histories. END may run independently on a coherent common checkpoint.
4. Produce the actual catalog, freeze one matched bundle and execute the four
   named non-cold Persona subset. Verify selected spec, tools and rendered memory;
   run a bounded real DeepSeek smoke, then launch fresh chat with durable memory
   and authored files preserved. Do not call the subset the full cohort.
5. G3 adds the remaining force/fork/callback transfer/worker-loss/full recovery
   matrix, bundle-owned M1–M3, performance measurements and reconciled pushed main.

## 1. One native output contract

Every `NativeOriginals` compilation uses the paired target/package output,
including finite source installers, startup, reload and catalogs. Only explicit
`PortableExecutable` uses a closed full program. Represent final output with
one exhaustive sum; remove dual maps and optional World fallback.

Native compilation receives opaque provenance from the actual CompilerScope
producer and validated ServicePackageAssociation profile before compilation.
World admission additionally checks that owner against held World facts.
Observation-only configuration cannot mint native certificates: select an
explicit portable operation or refuse. Do not infer provenance from a label.

Native build actions use the declared, action-owned compiler service as well.
Their endpoint binder requires that actual owned service and cannot fall back
to direct compilation. Portable fixtures retain their isolated direct path.
The action owns service settlement and retains primary failure separately from
uncertain cleanup; a timeout must not be reported as confirmed settlement.

Assign full binder identities before filtering. Preserve completed HOME
components, structural signatures, representations, literals, constructors and
site evidence. NativeReply's exact package obligations and seals constrain the
captured interface bytes consumed by final product issuance. Cross-image
contracts compare structural facts, not coincidentally equal local indices.

Package interface custody includes the union of actual canonical sidecar and
native receipt obligations, including direct facade imports and TYPE-only
dependencies without executable package products. Capture those selected bytes
once; canonical interfaces and Originals retain their respective obligation
subsets through the same immutable interface-token owner. Do not retain the
whole compiler output or infer executable demand from those subsets.

Keep the sealed selection origin, the captured storage path and the owned bytes
distinct. An authenticated selected interface may move without changing its
Original ID, certificate or selected-origin facts. Fresh and recovered owners
establish that relation; a path alias or equal digest alone is not authority.
Materialization must preserve package interface bytes before producer custody
ends, and recovery must acquire and verify them before validating their users.
Use required package-interface references in Original and canonical recovery
records, with the existing enclosing format cutover. Older missing-field records
refuse; there is no ambient-file fallback. Share byte tokens and existing
content-addressed writes rather than duplicating payloads for each Original.

## 2. Issue cohort availability once; admit each stage separately

Physical cohort admission owns new dependency-proof issuance. Move it out of
`normalize_selection`; subsequent selection normalization carries completed rows
and offers without re-solving the cohort or temporarily promoting namespace
roles. Every new output receives an availability receipt, including Empty.

Authenticate nominal requirements first. Resolve external terminals against the
same initial acquisition's actual contributions and recorded choices, retaining
the selected full binding and its contribution owner together. If no choice is
recorded, require one genuine issuer. Zero permission is unavailable; a missing
recorded choice, competing issuers, corrupt evidence or wrong acquisition is a
typed failure, not a Partial result. Equal carrier pairs and ordinals do not
identify a group issuer.

Classify local edges and resolved external terminals once. Propagate unavailable
dependencies to their local dependents; cycles with supported dependencies remain
eligible. Issue the supported graph through the existing checked solver. Evolve
the checked-custody offer issuer to preserve solved Source causes for Empty,
Partial and Full, rather than using the generic unwitnessed offer constructor.
Validate only advertised roots and their exact closures; physical body count is
not executable availability. Transfer checked rows and own offers directly,
without a temporary Original-role projection.

Availability uses legitimate initial offers, not only current stage demand:
unused `f = X.g` must remain callable later when its original acquisition permits
it. Current-stage execution checks actual exact Original uses before reusing a
Completed product, recursively through reached fresh Source groups. An inherited
sealed parent follows its issued children without creating new direct stage
roots. Prior Source metadata alone is not sealed executable selection; the
compiler must still traverse a previously unused group when first demanded.
Keep these capability paths distinct in private constructors, not a caller-owned
boolean precheck.

The initial external native selection remains bound to its original
`RequestCompilerInputs` and `InventoryOperation`. Retain the complete
`ResolvedNativeDemand`, including its demand owner; extracting groups alone
loses the contract. Existing same-owner guards remain strict.

Extend the existing private `WorldProgramContinuation` to compose that immutable
initial owner with actual issued prefix additions. A private stage-input
constructor receives the actual program, stage, parsed output and validation;
its result owns the initial basis, actual immediate predecessor, authenticated
metadata membership and executable choices. Certification consumes that result.
Delete reconstruction of executable demand from all receipt metadata.

Historical external `OriginalBinding` references resolve through the held
initial demand. Earlier fresh stages use exact `SourceOwner` coordinates and
actual prefix products; they cannot be resolved against inputs issued before
they existed. Authenticated but unselected metadata retains its exact
original/interface pair, symbol and acquisition as a latent requirement.
Membership alone grants no execution and must not erase this information into
only a CachedHomeOwner/ordinal or PendingImportOwner::Source.

Haskell's active quiescence callback and final Rust certification must consume
this same owner-derived data. No nested compilation or frontend replay. A newly
reached binder sharing an existing package group is legitimate demand progress.

### Joined stage and nominal-reference migration

Complete this as one producer/consumer migration. Removing receipt-derived
demand alone is unsafe while module certification eagerly converts every
Original reference into an executable import. Keep ordered nominal requirements
with each authenticated group; project executable imports only when that exact
parent group is selected. Do not retain a whole request or metadata map per group.

The private stage constructor must first prove that its checked stage was
actually emitted by the same program and reservation. Only then does absence
from the demand record mean an empty initial native selection. An unknown stage,
an empty selection and acquired-but-unexecuted permission are distinct states.
Initial historical choices remain bound to the initial acquisition; current
source/TYPE support and prior sealed targets come from the actual predecessor.
Do not weaken same-owner guards to join those independently authoritative facts.

HOME6 already encodes the exact Original interface binding, Original binding,
and symbol. Finish that variant's semantics in the unreleased matched G2 reader
and writer; no new wire tuple, default or compatibility fallback is needed.
Keep membership and local byte authentication separate from permission to select
the referenced group. Existing package-interface custody remains independently
owned and versioned as described above.

Extend the existing hidden-custody traversal to retain exact binding nodes for
nominal requirements. Payload IDs are insufficient: a historical binding can
share its payload with a different currently selected binding. A payload-level
`initial.contains(id)` exclusion must not drop that historical node. Use the
existing custody roots and cold verifier, not a parallel map or new graph label.
Publication must retain actual issued nodes instead of resolving every ordinal
again against a selected carrier. Current Source requirements retain their
separate source-validation rules.

A complete body census does not prove Full executable availability. Preserve
existing Empty/Partial/Full distinctions; selected groups require their actual
issued closure, while unselected nominal metadata retains custody without
acquiring execution. If current publication promises cannot express that
distinction, resolve the promise at its owner before changing consumers.

This tranche does not automatically complete a partially available parent when
a later request acquires more child permissions. Its published subset remains
the subset actually issued. Selecting another parent group requires a genuine
new parent proof/offer and an explicit choice; child permission alone cannot
manufacture that offer. If an ordinary G2 history requires demand-time parent
completion, retain that counterexample and design the transition in the existing
issuer before adding it. Do not discover availability by attempting every group
and swallowing refusals, or relabel an old parent as a newly compiled product.

Required joined evidence includes an unused authenticated but unpermitted
Original reference; selected historical G-old beside same-interface G-new;
the same payload under distinct binding scopes; actual multiple World stages;
parent-only projection and cold recovery after disposable predecessor removal.
Assert exact old execution, no native permission from metadata, refusal of an
unknown stage/foreign acquisition, and unchanged prior state on refusal.
Component passes do not replace this real producer-to-runtime history.

`original_reference_boundary` owns the nominal group/HOME/requirement relation,
exact custody, cold checks and availability migration. `stage_input_design`
owns the private World stage composition and deletion of receipt-wide demand
reconstruction. They share one contract and join on one feature branch before
root integration. Review the private owner shape before changing all consumers;
compile each coherent slice early. Neither half is independently G2-complete.

## 3. Completion survives every successful item

Add immutable `IssuedNativeSelectionCompletion` to existing
`CompilerSourceSupport`, separate from immutable Original certificates. It owns
full HOME rows/slot-to-package associations, exact package graph bindings and
hidden retained requirements. Use inherited inventory binding issuance so two
callers or siblings retain the same unchanged package binding as demand grows.
Do not introduce per-root graph hashes or runtime graph-equivalence guesses.

`OriginalCompilerInputs`, `RequestCompilerInputs` and their transformations
carry the same completion references. TYPE-only composition preserves receiver
completion but cannot manufacture readiness from unrelated incoming custody.
Version required materialization/recovery facts explicitly; missing completion
fields do not mean a complete empty set. Artifact bytes alone do not establish
HOME-slot/package association or executable selection.

One coissued checked successor carries both the World and the actual advanced
compiler context/completion through successful runtime settlement. Atomically
update `Tip.exact_context` and `Tip.inspection_world`, including zero-name
expression cells. Rebuilding an evicted World must recover the same facts.
Runtime commitment may change while the narrow Haskell TYPE/name input identity
remains unchanged. Do not widen that identity just to carry runtime custody.

Prepared-source publication and capture derive from the actual held
`Arc<SealedTurnProducts>`, including late completion, not merely its prior
`SealedOriginalCompileInput`.

## 4. One native recipe and scoped instance custody

Generalize the existing immutable recipe to native imports that are either exact
local HOME/package references or typed retained-value requirements. Keep one
coupled product/image owner and ordered import contract. A closed recipe is a
checked restriction of that same plan to no retained requirements, not another
planner. Reusable plans never contain live handles, scopes or source leases.

The per-install owner resolves retained slots against actual BindingEntry
identity, generation, representation, signature and custody. A required signature
is not evidence that a retained handle provides it. Preserve unsupported Address
capture refusals. Extend NativeDomainSelection from the same BindingTable view
with authored domain associations; choose dependency domains from that owner,
not by assigning every import the current domain. Reused instances preserve their
original environments and ProgramFacts.

Move ordinary checked cells, captured values/functions and host mounts through
this common installer. Explicit finite module production remains supported;
ordinary checked private settlement already bypasses finite retraction and must
not acquire a fallback to it.

`PreparedSourceEntry::prepare` issues a reusable, immutable
`ClosedInstantiationPlan`: exact HOME/package occurrences, held images and
products, ordered private export references, target and root requests. It has
no live Existing variant, scope, runtime handle, temporary ROOT session or
second resolution step. Actual installation binds fresh state.

The common exact logical binding key distinguishes full `NativeGroupBinding`
from exact package `ArtifactBinding`, paired with actual `SourceInstanceDomain`.
Physical roots retain group instance, ValueId and actual handle/signature.
HOME and Package have exhaustive lease evidence; packages never masquerade as
CachedHomeOwner/SourceBinder. Runtime keeps acquired product/binding proofs in
existing ProgramFacts; codegen consumes validated scalar facts.

Immutable HOME image preparation takes certified code and literal contracts,
without requiring a runtime instance identity. Proving a selected closure has
no live captures is separate from compiling its images. An open target can
contain a closed package subgraph: its exact held PackageCodeBinding and ordered
package-only links prove that distinction without requiring the whole target to
be closed. Pair that definition with the actual installing operation's admitted
source for freshly instantiated package sites. A domain selects the mutable
environment; its ID does not issue or recover source authority. Inherited HOME
rows must not acquire current-source authority through this package rule, and
reused instances keep their original ProgramFacts.

Actual A-to-B runtime captures retain A's source prerequisites before the
BindingTable view is seeded. Installation admits source work against that
captured dependency DAG before allocation; fencing A therefore refuses a new
operation even when a package instance has not yet materialized. Importing
immutable compiler evidence into an independent B is fresh B execution, not a
capture of A's live environment. Verify both cases through production admission,
including an effect site located only in package code. Keep the same
prepared-program owner through these stages instead of copying its full graph
to establish an association.

Extend existing BindingTable selection/custody, batch attachments/registration,
rollback, capture and retirement. No parallel instance registry. Installer I
and public notebook B are distinct domains. Repeated and sibling demand in the
same B shares B's package CAF; another B is fresh. Captured A retains its actual
authority and the corresponding cutoff/retirement refusals.

## 5. Complete the cutover

Delete `code_exports` advertisement, lookup, prefer-existing, raw fallback,
stage and publication routes. Delete cache-only RetainedPackage/CodeExport
wire and recovery variants. Real captured values/functions remain explicit
BindingEntry/Retained authority. Merely renaming retained package generation
to RetainedOwner while preserving implicit generation lookup is not deletion.

PackageCode has an explicit artifact kind. Non-native is not synonymous with
interface. Package products are not compiler-namespace conflict owners; two
package groups in the same module are valid. Issue ordered package dependency
edges through the existing inventory owner and validate exact exports.
The package graph owns only its reached package payloads and proof ancestry.
The enclosing completion explicitly combines those with its HOME custody;
constructing a package graph must not retain the entire parent selection.

Finish matched TPCERT11, TPHOMEOWNERS6, request29 and exchange framing, plus
required snapshot version cutovers. No silent defaults or legacy adapters.
Preserve user source and memory; rebuild compiler products with real producers.

Native tests select the actual owned service and its declared source profile.
The existing macro/generator/runner must reject inconsistent mode/resource
declarations. Mixed harnesses retain explicit Portable/direct cases and separate
declared observation and service manifests; mode selects the exact manifest
without fabricating or falling back from a supplied invalid service declaration.

The frozen catalog gate starts a fresh bundle-owned compiler inside its isolated
namespace. Admit only the exact standard Nix socket, an authored `store = daemon`
configuration, the frozen bundle read-only and private evidence at its canonical
host path. Preserve offline Nix and absence of checkout, Buck outputs, caches and
ambient compiler endpoints. Existing epoch acquisition/retirement owns the roots;
frozen retention is not an invented per-service receipt.

Reload reporting retains unknown source as unknown and actual blockers only.
Missing blocker observation must refuse, not become an empty successful census.
Keep existing publication ownership; qualify confirmed B plus caller loss using
an actual staged publication, separately from unconfirmed body-result loss.

## Work ownership

The current [root plan](plan.md#ownership-and-integration) assigns the eight
active lanes. Source workers retain isolated checkouts and exchange exact
commits; root owns canonical integration. The target, nominal and prior-stage
lanes join on one coherent feature before root acceptance. Runtime and Haskell
repairs can proceed independently but must pass their joined consumers.

Each owner preserves WIP, compiles affected production and cfg(test) targets,
executes nonzero focused controls, and reports exact source, artifact, command,
counts, outcome and logs. Source-ready and compilation-only are explicit states.
Independent review checks refusal paths, obsolete alternate entry points and
whether fixtures exercise production issuance. No invented test authority.

Shared constructor changes require a concrete consumer and coordinating owners.
Continue unblocked work; escalate a new semantic choice with its counterexample
instead of adding a fallback. Root integrates reviewed slices without waiting
for unrelated qualification, retaining honest checkpoint status.
