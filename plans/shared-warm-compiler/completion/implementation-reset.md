# Implementation reset: live facts carried to their consumers

Accepted 2026-10-20. Read with plan.md and contracts.md. This is the receiving
contract for implementation, not a claim of passing behavior.

## Haskell World environment

The failing mixed history reaches rnImports inside GHC Make's T_Hsc hook.
Initial downsweep setup succeeded; Make replaced the earlier environment.
Retain actual same-World HomeModInfo, including hm_details, instead of converting
all prior originals into interfaces and rehydrating them. Same NameCache/EPS
ownership is required; this is not cross-interpreter bytecode reuse.

At the consuming phase combine actual prefix Original HMIs, source-less Finder
locations/exclusions and virtual graph nodes/edges justified by real imports.
Preserve current phase entries; conflicting owners refuse. Instance/family
visibility needs graph reachability, not only HPT entries. Existing stage source
imports prior Original inventories; use that evidence, not a new graph system.
Values remain demand-driven. Keep genuine cold hydration; delete redundant warm
prefix hydration. Do not add old source files as Make targets or restore only
at cpAfterLoad. Preserve exact admitted item/source identity across retries.

Acceptance includes bind -> declaration capturing bind -> later use, Original
reuse, instances/families, hidden implementation visibility refusal, generated
retry and later-type-error zero effects. Execute existing RequestInputs and
transitive-publication controls; their earlier compilation was not execution.

## Rust physical issue and item assembly

CertifiedSegmentOriginals retains final normalized OriginalCompilerInputs from
its existing issuer, with completed selection and exact group-binding ownership.
Do not return an earlier NewProduct draft before finish_new_product_availability.
Derive bound selection/indexes from the same admitted physical result once.

Physical bytes/authentication, new body qualification, inherited exact choice
validation and physical indexing happen once per stage. Per item retain genuine
entry/import validation, visible produced-prefix projection, newly demanded
binding and settlement. Compiler continuation consumes compiler support;
executable target consumes executable demand. Remove from_selection rebuilding
from target views and CachedHomeOwner/metadata -> ArtifactId reconstruction.
Keep broad physical custody local; do not attach unrelated future items to every
value. No new certificate issuer, registry, cache or generic trusted constructor.

The physical-selection fixture must first produce its claimed operand capture:
qualified import of the actual operand module plus an unreachable helper RHS
referencing the genuine binder. Typed root can retain constant lets. Assert
capture first, then visible-prefix/no-runtime-demand behavior. A used case must
reference that same binder. Keep actor Some as the actual renderer/runtime gate;
a lower toolchain fixture must not introduce an upward crate dependency.

## Native planner

Use existing PlannedTarget::ExactGroup / NewGroup / Artifact throughout.
Issued/CurrentIssued acquisition carries a full exact binding; NewCurrent plans
its own typed dependencies. Historical descendants stay closure nodes, not new
admission roots or prebound logical-key choices. Resolve an unqualified request
once using its existing selected/proof candidates, otherwise refuse ambiguity.

Delete unconditional descendant promotion, generic historical fallback for a new
group, redundant prior-root seeding and later masking repairs. Inherited snapshot
custody already exists. Preserve current canonical/original namespace constraints
through existing world_primary_type_row bindings without adding Group roots.
NewCurrent classification comes from actual acquisition, not absence in parent IDs.
Keep complete exact-edge reuse fixed point: same complete binding may reuse;
changed dependency scope requires a distinct row.

Required counterexample: old R8 -> G7old -> ValuesOld, fresh G7 -> ValuesNew,
fresh J9 -> exact old R8. Planning G7new must not resolve to G7old. Also cover two
historical roots, empty-interface zero new group roots, explicit later child
without own proof, namespace preservation, identical-row reuse and changed-scope
nonreuse. Stage-specific body refusals stay meaningful.

## Remaining native and source boundaries

Carry the same `IssuedOriginalOutput` exact binding through compiler-input roles,
custody, availability and decisions at all five consumers. Matching canonical
bytes do not establish equal issued bindings. Do not reconstruct a binding from
broader inventory, union unrelated views or weaken the guard. W's five-consumer
migration remains source-only with incomplete tests.

Mixed reuse must preserve phase-correct same-World completion environment,
demanded prior source bodies and the actual Original worklist. Fix the Rust
same-program repeated-SourceOriginal guard without silently duplicating native
issuance. SOURCE-cycle source-body restoration and ordinary mixed reuse remain
separate boundaries. H/engine and R own those respective repairs.

An initial authored runtime exception is an ordinary `Rejected` result;
`Delivered` describes a result after an actual effect-response resume. Root's
wrong-payload control must discriminate that boundary without accepting success
or infrastructure failure. Host-binding fixture preparation must carry the same
checked rendered-source observation, rather than compare authored bytes with a
larger generated module. Current exact runs and pending qualification are in
[the verification ledger](tests/verification-ledger.md).

## Reload core and public route

Use one affine retained core plus transient driver, issued together privately.
Core owns frozen B, expected A, reservation, body evidence, exact fence, captured
owners and independent publication/source/cleanup/diagnostic obligations. Driver
owns callables, inventory access and reply endpoint. Transfer same core on owner
loss before disposing driver outside locks. Preserve real before-body/entered,
observed/lost, awaiting/captured, held/reopening, Bconfirmed/Asettled distinctions.
Delete mirrored phase/default enums and receipt-as-state translations.

Actual Reload submits unreserved preparation to RunState; owner reserves and
retains before await. Prepare/validate B first. Fence A, settle pre-cutoff transfer,
capture real dependencies. Default blockers cause zero stop/publication and
truthful B-then-A restoration. Empty capture performs actual linked publication,
B outcome and A retirement. Delete direct bypass/PublicationNotQualified.
Remove publication CompilerOwnerScope after verifying helper graph: publication
uses checkout/store/frozen evidence; compiler END belongs to B preparation.
Remove ignored bootstrap claim if call graph confirms no semantic use.

Same-fence typed error repair65c930 is a reusable candidate, not tested acceptance.
Never discard its returned fence when B is committed but A retirement is pending.

Commands decoded to owned Rust data are generation-independent. Workbench awaits,
watch callbacks and SourceBinding closures may retain actual A code. Do not stamp
all processes with source generation. SourceBinding's stable existing slot owner
must enumerate/capture actual closure+execution+connection guard. Pending attach
keeps its source claim; final capture follows pre-fence attachment settlement.
Selective close preserves slot indices/typed tombstones, settles late events and
never detaches independent B slots. Temporary fencing retains/defer accepted
input through existing owner; no mapper replay or wait under machine borrow.

Early gate allows explicit force refusal. Full force captures real cleanup
capabilities, final physical closure/collateral check before first stop, exact
self-reload continuation retained, independent LLM worlds preserved. Unsupported
owner families are explicit blockers, not an empty census.

WorkbenchExecutions is memory, not a durable journal. ActorRecoveryJournal owns
reload/stop intent and outcomes added as needed; RunRecoveryStore stays the sole
selected-generation authority. Persist intent before irreversible stops, with
existing operation identity, A/B and captured set. No new journal/serialized
continuation. Uncertain effects never replay merely to obtain acknowledgement.
A reopened source stays reopened even if diagnostic delivery remains unknown;
diagnostic ACK is not physical cleanup evidence.

## Cross-area review and deletion

One production history is the early integration oracle, before broad suite runs.
Physical/canonical custody, interface compatibility, selected implementation,
issued edges and executable demand remain distinct in fixtures and production.
Queries share actual compiler world and preserve view fences; no reconstructed
finite query path. Remove finite resident compilation and its test-only routes
alongside replacement, preserving distinct legitimate build/cold consumers.
All current API changes compile production and cfg(test) consumers and execute
meaningful positive/refusal cases. Review exact joined candidates independently.
