# G2: complete the native compiler/runtime join

Accepted implementation contract, 2026-10-20, based on integration checkpoint
`e78e1152c1847f72b7442e02c2e05b5c48d90a73`. Controlling branch:
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

## 2. Compose issued stages; do not re-resolve initial evidence

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

## 4. One closed recipe and scoped instance custody

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
no live captures is separate from compiling its images. Only that closed
selection and the current operation's admitted source can assign source custody
to a newly created instance; reused instances keep their original ProgramFacts.
Keep the same prepared-program owner through these stages instead of copying
its full graph to establish an association.

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

## Work ownership

| Owner | Primary responsibility | Adjacent review |
|---|---|---|
| Root | Shared contracts, stage/completion scaffolding, joined review, consolidation, G2 | Wrong-owner paths; needless reconstruction; honest tests |
| E / engine_completion | H demand callback/writer and selected package-interface custody | TYPE-only witnesses; finite/item writer parity; strict cutover |
| common_native_output | Native physical output owner, finite/Turn layouts and required-owned build endpoint | Actual producer controls; primary-error retention; no raw sealing fallback |
| N / native_offer_scope_repair | Completion transforms and required v7 snapshot restoration | Zero-name cells; cold reacquisition; stable inherited bindings |
| Q / audit_native_selection | Package graph/binding issuer; ordered edges; kinds/storage/recovery | Namespace separation; real positive round trip; forbidden alternate issuers |
| W / review_world_lifetime_architecture | Common codegen custody, exact domain keys, leases, attachments, BindingTable | Rollback/retirement; structural export contracts; stale cache APIs |
| L / audit_reload_ownership | Closed recipe, runtime link/adoption, sealed-output consumers, fallback deletion | Atomic tips; captured-A refusal; caller-loss visibility |
| T / audit_warm_lifecycle | Production histories, fixture repair and G2 qualification | Independent oracles; source/public scope distinction; counted execution |
| X / compiler_demand_transport | Atomic World prefix carry and stage/prior-target composition | Both ordered-link checks; unchanged namespace; actual mixed histories |
| original_reference_boundary | Coupled authenticated metadata and executable Original selection | Exact nominal references; foreign-owner refusals; latent HOME/recovery follow-up |
| native_build_service | Declared native build service and retained action evidence | Portable isolation; settlement uncertainty; dependency fanout |

N owns compiler_projection support transforms and HOME-slot completion
associations. X owns World continuation and its artifact cursor; root owns the
stage-input contract replacing broad receipt-demand reconstruction. The Original
reference worker coordinates its prerequisite selection owner with that contract;
latent HOME/recovery changes require a separately reviewed complete producer and
consumer migration. Q owns artifact inventory, cold validation and
declaration_context materialization/recovery sections, coordinating edits with N.
W owns codegen demand/BindingTable/batch contracts; L owns runtime preparation,
selection, snapshots and publication. E owns H CompilerRequest/CompilerProducts;
X sends raw-worker guard requirements to E instead of overlapping those edits.
Root freezes constructor signatures with owners before editing their hot files.

Each owner preserves existing WIP, compiles production and cfg consumers early,
executes focused nonzero cases, reports exact base/tip and evidence, and sends
review-ready/blocker/settled events. Reuse warm checkouts under admitted resource
limits. Independent branches are not joined acceptance.
Bounded aesthetic improvements may proceed alongside this work after tracing
their production consumers and reviewing effects on ownership, work, failure
behavior and API simplicity. They do not substitute for the joined G2 history.

## Integration sequence and decisive evidence

1. Freeze shared owner/recipe/lease/successor signatures and integrate reviewed
   existing prerequisites. Compile joined compiler and runtime consumers early.
2. Run a real finite source → PreparedSourceEntry → installer I_B → public B
   World → two ordinary B package-using cells history. It must carry the same
   completion-bearing context. Assert exact package binding and B-cell CAF reuse;
   I_B/public B are distinct physical instances unless actual transfer exists.
   Then retire captured A and run another B cell. No synthetic HOME forwarding,
   guessed ValueIds, second machine or raw compiled().code route.
3. Extend the existing hosted ordinary demand history with bindings,
   declarations, explicit/empty imports, expression/mixed cells, source-defined
   types, retained functions/actions, later use and predecessor cache eviction.
   Preserve immutable required custody while deleting only disposable outputs.
4. Execute actual paired writer→reader→closed-linker behavior against GHC;
   assert package bodies are absent from target definitions. Cover two callers,
   siblings and historical R→G-old beside independently selected G-new.
5. Execute discovery's five verbs and stale view refusal, used/unused/crossed/
   retired private inputs and compile-before-effects. Exercise two-stage demand,
   mixed known/fresh demand, progress without new roots and exact seal refusals.
6. Default reload uses real staged frozen B despite draft C. Preserve captured-A
   refusal separately. Inject caller loss only after observing actual typed
   publication; B stays callable. Cancellation before/through publication keeps
   truthful outcomes. Force may refuse without damaging later ordinary reload.
7. Run affected compiler fixtures. Freeze one matched bundle and use existing
   exact-test selection for the four non-cold persona-startup members; label it
   a four-member subset, not the full seven-case cohort. Then real DeepSeek smoke
   through those same frozen bytes. Offline HTTP controls are not live evidence.

Record actual source/artifact, command, selected/executed counts, exit and log.
Update the compiler work map against the joined source. Separate structural
savings from measured latency; this wave closes the ordinary workflow first.
