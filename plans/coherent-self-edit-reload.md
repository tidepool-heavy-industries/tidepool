# Coherent self-edit reload

Accepted implementation contract, 2026-10-13. Coordinator branch:
`persona/selfedit-delivery-20261013`, starting at `1a6fdf943c`.
Current main `46aeb2d7ab` is an ancestor. Preserve the live Persona and all
foreign WIP. Supersedes the unqualified parser/namespace-overlay experiments.

## User-visible behavior

Ordinary files hold reusable Haskell definitions, workspace prompts, and memory.
One explicit `reload` activates edited source, compatible AgentSpec handlers,
workspace prompts, and a fresh notebook. Haskell `reloadSource [additionalModules]`
adapts to that same operation. Reload does not commit files to Git; ordinary Git
operations own version-control publication. Remove separate helper/spec publication implementations and
migrate callers; compatibility shims are not required.

Changed configuration resets notebook names; unchanged configuration does not.
Failed preparation or cancellation before commit preserves the active generation.
Conversation, published memory context, actor identity, and owned resources persist.
Reload does not rerun initialization or implicitly refresh memory. Prompt-only
changes count as changed configuration. Memory contents do not enter that key.

Existing requests and children retain their issued generation. Fresh-context
children select the published source. Explicit adoption of parent state is deferred;
captured-only children remain frozen. Git/rebase/reload applies to source-backed
actors with an editable checkout, not to captured runtime state.

Registered tool surface and typed actor protocol compatibility remain required.
Source roots, package pins, service configuration and capabilities are launch inputs.
New recovery formats may refuse old runs; old live runs retain their matching binary.

## Shared contracts

An opaque actor-owned `ActorGeneration` retains a generation ID, complete immutable
source graph, installed tool lease, composed prompt snapshot, and notebook scope
lease/publication owner. Only prepared components can construct it. Actor/session/
resource identity is independent from notebook scope. Requests capture the whole
generation once; nested work, source inspection and completion preserve that owner.
Never reselect the current generation midway through an issued request.

Runtime recovery owns typed generation-qualified public ownership and active
generation selection. Reuse RunRecoveryStore/PreparedRecoveryWrite and its native
installation acknowledgement; do not add a journal or independent head registry.
Persist exact source, installation and prompt references, not display labels.
The actor layer retains live leases; runtime serialization must not depend on the
actor crate. On restart, the existing incarnation-transfer transaction transfers
the actual active generation and its exact references, rather than assuming
generation 1. This startup transfer is distinct from reload and keeps its existing
incarnation invalidation semantics. Runtime/source/actor owners agree their concrete handoff before edits
to shared signatures; report changes to the coordinator and dependent workers.

Publication: durably prepare immutable inputs and isolated scope; fence expected
generation; claim PublicationDecision; commit generation/head atomically; install
the already prepared live generation; acknowledge native installation. Source
symlinks/status files are derived projections repaired from durable state. An
uncertain commit blocks admission and reconciles; it never authorizes blind replay.
Late A publication targets A, never B. Release old scopes only after leases end.

The notebook generation ID is nonzero, initially 1, and the durable owner issues
successors. Initial captured-child admission may retain inherited state; only
reload replacements must have an isolated empty notebook. Do not apply the
replacement emptiness rule to ordinary captured-child startup.

Staged source exposes immutable prepared inputs, not a second commit operation.
The active actor generation and durable head provide the stale-writer fence;
derived symlinks cannot supply a conflicting authority. A projection repair
failure after a confirmed transaction reports the committed generation and a
repair warning. It is not a rollback or evidence of database uncertainty.
Unrelated domain revision changes may rebase the prepared immutable write when
the expected active generation remains unchanged; they must not repeat compilation.

## Source and compiler

Cut old workspace-source fallback and restrictions justified only by it. A selected
revision contains the complete workspace/helper graph plus explicit immutable
shared libraries and runtime resources. Add/edit/remove modules inside declared
roots; removed modules cannot resurrect from an older source tree or cache catalog.
Missing required imports fail normally. Keep reserved module ownership checks.

One existing native installer candidate compilation validates configured, changed/
new, helper and also_check modules and prepares handlers. Use validated imports in
the existing builder/cache key. Remove the preceding duplicate check_source pass.
Candidate preparation consumes the full staged graph, including helpers. Builtin
readiness must not bypass changed-source validation.

Installer originals remain private to its installation scope. Notebook B is minted
with mint_isolated_scope and retained through the existing scope lease; never copy
notebook A imports/facades/projections or publish the whole installer closure into B.
Normal notebook compilation acquires source originals lazily. Preserve exact typed
request evidence and runtime contract imports; conflicting nominal seals refuse.
Keep compiler/machine/cache owners and declaration allocation counters alive.

## Prompt publication

Compose captured workspace base/agent/API-guide text with host-owned invariants.
Use the same ActorGeneration snapshot for instructions and tool/source authority.
Harness request_snapshot precedes instruction interning and request construction;
transport and replay use identical captured instruction bytes. Retries retain them.
Compaction uses the same selection rule. EngineConfig.instructions remains only
the generic-client fallback. Do not rewrite conversation history or use user-message
context editing as an instruction publication mechanism. Preserve cache affinity.

## Parallel owners and acceptance

- Actor owner: generation issuance, isolated notebook scope, unified reload dispatch,
  late completion, lifecycle and lease cleanup.
- Runtime owner: generation-aware durable ownership, one recovery transaction,
  recovery/fault injection/properties. Coordinate types with actor owner first.
- Source owner: complete source graph, staged capture/prompt inputs, one compile,
  derived source projections; coordinate shared staging API with actor owner.
- Prompt owner: Harness snapshot instructions/replay/compaction and facade adapter;
  independently commit and pin the reviewed Harness result.
- Acceptance owner: actual self-edit history and display-page oracle; source edits
  through production APIs only. Release qualification follows joined acceptance.

Each worker owns an isolated checkout and compiles affected production and test
consumers before delivery. Admit parallel builds against actual headroom; at start
host has ~50 GiB available and build.slice uses ~54 GiB of its 96 GiB maximum.
Preserve scope evidence, exact commands/counts/revisions. No worker merges or deploys.

Required histories: edit/reload/use; changed vs unchanged; new/deleted modules with
warm caches; invalid source and incompatible contracts; A issued then B published
then A completed; old generated module import refusal; pinned children; prompt A/B
across inference, retries and compaction; crash before/after commit; cancellation;
resource and memory/context preservation. Independent shrinking operation-history
models cover issuance/publication/recovery without running GHC for every case.

Repair the joined display assertion using its retained output owner, not aggregate
textSource output or substring trimming. Preserve previously qualified persona
parcels. Exclude overlay/parser WIP and unqualified debug instrumentation.

Run the joined production history early. Then qualify one exact matched bundle,
smoke with Sol 6.1 medium, and launch replacement Persona with Astra medium while
preserving authored files. Lookup optimization remains a separate background task.

## Overnight completion wave

Delivery outcome: a qualified, self-editing Persona available for human use with
Astra at medium effort. Keep the current live run until its replacement passes.
Finish this campaign; do not start another broad audit or performance redesign.
The joined actor/runtime/facade consumer build at `26bc97022c` passed. The source
history property is integrated at `8ac93a9eb5`; its owning run passed 64 fresh
histories and a persisted counterexample replay. Compilation is not hosted
acceptance. The remaining release blocker is changed type declarations sharing
unversioned constructor identities across retained generations.

### Why type identity needs repair

The session metadata table is not merely a cache of the latest program. Values
can outlive their producer, cross a callback or parcel boundary, and appear inside
later results. Their host constructor IDs must still identify the correct type.
Native cases use descriptor identities; the machine interner and family checks
must distinguish versions too. Clearing metadata, keeping only per-program
tables, accepting shape collisions, or appending arity to an ID would lose those
contracts. Qualifying everything by actor generation would unnecessarily split
unchanged library types and prevent legitimate cross-program sharing.

Use the exact defining GHC declaration ABI. The seven-variant pinned 9.12.2
experiment in `/srv/swarm/evidence/inanna/ghc-tycon-hash-20261013/RESULTS.md`
establishes that `mi_hash_fn` covers the tested private, unused, recursive,
abstract, newtype, dictionary, data-family representation and promoted cases.
Arity and same-layout field-type edits change fingerprints; comments and an
unrelated term edit preserve them. Reading retained interfaces reproduces the
fresh fingerprints. This is existing compilation output, not another compile.
It is an ABI discriminator, not authentication: exact original provenance and
all existing representation checks remain independently required.

### Shared identity contract

Settle the concrete names in one reviewed schema commit before parallel edits:

- Keep general `SymbolIdentity` unchanged for term selection and provenance.
  Add `TypeIdentity { symbol, version }`, with a closed version type for
  compiler-known wired-in types versus a 128-bit GHC declaration ABI fingerprint.
  No optional version, unknown-version default or module-name string suffix.
- Add `ConstructorIdentity { symbol, family: TypeIdentity }`. The family is
  derived from this identity, not separately stored in a field that can disagree.
  Constructors of one algebraic family share its version, including unchanged
  sibling constructors when another sibling changes. Promoted constructor names
  retain their own nominal spelling and resolve through the defining parent ABI.
- Resolve an occurrence only after selecting its exact defining `Module` and
  immutable interface owner. Keep version evidence scoped to the prepared owner;
  a mutable process-wide `Name -> current version` map is invalid for A/B reuse.
  Preserve the existing whole-interface original-admission checks separately.
- Fresh `FinalizedModule` already carries `HomeModInfo`; recovery already reads
  the defining interface. Issue a compact immutable identity resolver from these
  owners and carry it through `PreparedModule`/projection. Imported types use
  their exact selected defining interfaces. Missing evidence is a typed compiler
  boundary failure with owner/name diagnostics, never a textual/shape fallback.
  Resolve demanded identities once per immutable owner, sharing the existing
  interface snapshot; do not reread files, retain a mutable HscEnv in thunks, or
  eagerly expand every type in every package. Prepared-cache hits must retain
  their original resolver, not borrow the next request's current interfaces.
- Use one issuance function for constructor declarations and metadata. Retain
  the issued constructor evidence with its GHC `DataCon` through artifact
  assembly; sidecar production must not reconstruct identity from raw names.
- Qualify constructor host IDs from the complete typed identity. Preserve the
  matched compiler's existing wired-in ABI where host primitives require it;
  use actual GHC wired-in evidence, not spelling or `main`-unit heuristics.
  Use framed, domain-separated input and preserve the reserved tag/ranges and
  array/closure/oversize observation sentinels. Numeric collisions still refuse
  before mutation. Do not repurpose term VarIds or make a second Rust issuer.
  Concrete new-ID rule: SHA-256 (already a compiler dependency) of the bytes
  `tidepool-constructor-id/v1\0` followed by the schema owner's canonical CBOR
  constructor identity; take the first seven digest bytes in big-endian order
  and retain the existing `0xFE` high-byte tag. GHC fingerprints encode as two
  big-endian Word64s, never pretty-printed text. The schema owner specifies one
  encoder, with cross-language semantic decode controls; the compiler owns ID
  issuance. Full typed identity remains authoritative despite the truncated ID.
- Carry `TypeIdentity` through algebraic case families and nominal type-graph
  declarations, and `ConstructorIdentity` through metadata, templates, descriptor
  interning and host answer validation. Human-readable names remain diagnostics.
  Same identity with conflicting layout remains an error.
- `DefinitionFacts`, `SiteTypeEvidence` and its manually encoded commitment must
  retain the complete identity, including family/version; bump the commitment
  domain. Audit constructor reply tables and settled/freer/JSON role selection.
  Spelling indexes select candidates only: exact consumers compare typed identity,
  and ambiguous versions refuse rather than selecting the first matching name.
  Audit type-graph evidence framing, cursor equality and work accounting as well
  as the top-level schema. Test unchanged-type descriptor sharing across two
  independent machines and value transfer after the original image retires.
- Bump the owning prepared schema, execution ABI and metadata versions together.
  The schema owner records an exact producer/consumer/version matrix from current
  source (currently prepared 17, ABI 9, TPLR 5.0). Audit group/native containers
  for embedded identity encoding; bump those whose own grammar changes. Reject
  old inner payloads, regenerate fixtures, and use the new producer cache key.
  No compatibility shim, global cache deletion or rewrite of the old live run.

### Publication completion

Fix the actual ordering seam, not only the observed notification order:

- Publish lifecycle state from the complete issued `Arc<ActorGeneration>` rather
  than three independently supplied context/owner/installation fields. Within
  one actor incarnation, processing a delayed older notification must not replace
  the newer generation. Equal-generation disagreement is a contract failure.
  Retained requests still carry A; this ordering rule applies to current state.
- Source reads and captures consume issued source capsules. Fresh startup must
  capture its boot selection once; cold startup consumes the persisted selection;
  children/checkpoints consume the appropriate issued parent selection. Remove
  fallback selection from projected `active` links wherever these exact facts
  already exist. Source-free operation stays an explicit supported case.
- Eliminate reload-time filesystem projection after migrating those exact
  consumers. `selected-generation.json` has no reader and should be removed.
  Keep initial `SourceLayer::publish`/prelaunch seeding and initial preparation;
  distinguish boot selection from selection after a generation has been issued.
  In particular, repair the child fallback in `child_launch.rs`, the checkpoint
  fallback in `resident_actor.rs`, and fresh initial source capture before
  deleting `finalize_projection`. Do not add a second durable head or registry.
  Replace tests asserting that `workspace/active` moved with assertions about
  actual issued source. Update source.rs's obsolete symlink-publication comments.
- After-tool failure deduplication belongs to the captured installation, not a
  manual reset performed only by the native reload adapter. Key it by the
  existing actor-local installation identity plus reason; a late A failure cannot
  suppress a B warning. Derive install counters from the same captured owner.

### Worker contracts and sequence

Use isolated worktrees from this delivery branch, Sol 6.1 for implementations
and review, Luna for bounded consumer inventories and simple test migration.
Model-capacity errors are stopped turns, not running work: reassign bounded work
to available Sol/Luna capacity. The coordinator owns contracts, reviews and merges.

1. **Identity/schema owner:** `tidepool/repr` and the pure Haskell execution-schema
   package. Issue the shared types, encoding/version changes, validators and
   representation fixtures. Publish the concrete API first. Add cheap generated
   identity/round-trip/collision histories; compile owning Rust and Haskell tests.
2. **Compiler owner:** `FinalizedModule`, `FatIface`, `PreparedStg`, projection,
   metadata and compiler tests. Consume the shared API, retain exact per-type
   evidence through fresh/recovered paths, and remove duplicate name reconstruction.
   Fold the retained seven-variant probe into existing GHC test infrastructure.
   Do not add a new framework or production compiler pass.
3. **Native/runtime owner:** descriptor interner, codegen family checks, runtime
   metadata/host conversion/type-graph consumers. Consume the same identities;
   keep pooled descriptors shared for unchanged types. Preserve failure-atomic
   admission. Add same-machine A/B coexistence and nested old-value observations.
4. **Actor/source owner:** publication ordering and exact source selection above,
   independent of constructor plumbing. Own the common notification boundary,
   remove stale projection/fallback authority, and unify after-tool ownership.
   Compile both actor and facade callers, including test consumers.
5. **Acceptance/release owner:** maintain the existing hosted reload and cold
   recovery histories, correct any test-oracle mistakes by tracing production
   contracts, and prepare exact commands/resources before the joined build.
   Run the first real history as soon as the compiler/native pair joins; do not
   defer all integration feedback until the final bundle.
6. **Independent reviewer:** review identity admission, captured A versus current
   B, failure/cancellation atomicity and cleanup. Search immediate analogous
   consumers of the changed boundary. Report concrete findings with owning paths;
   do not start an open-ended repository audit.

Owners 2/3 can inventory consumers and author regression fixtures while 1 settles
the shared API. Owner 4 and acceptance preparation run independently. Merge small
reviewed parcels into the delivery branch regularly; no worker merges into main
or deploys. Compile affected production AND test consumers in each lane. Parallel
build admission follows actual host/cgroup/disk headroom, not a single build owner.

### Discriminating tests

Keep inexpensive component properties separate from expensive compiler histories:

- Fresh versus retained-interface issuance yields identical identities. Different
  units, changed arity, changed same-layout field type and changed recursive
  dependencies differ; comment-only/unrelated term changes retain the tested
  type identity. Hidden/class/family/promoted cases are required fixture partitions.
  Include actual imported package hydration and a reexport (for example Text and
  the effect library), since the retained experiment covers home interfaces.
- Runtime histories install A, install B, read/call both, retire one owner, transfer
  a nested retained A value, refuse a conflicting same-identity declaration, then
  continue using valid A/B. The independent model keys logical type revisions,
  not production hashes or interner handles. Include ABA reuse and unchanged
  sibling constructors. Show the original unversioned implementation fails.
  Also cover changed request/reply families and stale A settlement. An unchanged
  sibling must acquire B's family version even when its erased field layout is
  identical; a same-layout A value must not satisfy B's typed reply contract.
- Generation notification histories deliberately process C before B and verify
  current lifecycle state remains C while issued A still observes A. Child/source
  capture must use C even with stale/missing derived links. Prefer barriers and
  operation histories, not timing sleeps.
- After-tool history reports identical failures under A and B independently,
  including a late A completion. Native reload and Haskell reload share behavior.
- Execute `actor_host::coherent_reload_acceptance_tests::coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work`.
  It must exercise real A/B type changes, callbacks, notebook reset, no-op,
  invalid-source refusal and extra checks through production interfaces.
- Execute `actor_host::embedded_recovery_tests::production_coherent_reload_recovers_published_configuration_without_recapturing_draft`.
  Verify published B under edited draft C, exact prompts/installer, retained
  bindings/conversation, initializer exactly once, and selected-artifact refusal.
  Recovery tests distinguish legitimate incarnation transfer from source/config
  replacement; do not require arbitrary full-record equality after a failed start.
- Run `just fixtures-check` for the changed translation/serialization boundary,
  and the scoped existing M1–M3/provider/callback controls affected by this wave.
  No zero-match pass, private authority constructor, manual SQL repair or obsolete
  fault framework. Record selected/executed counts and exact artifact identity.

### Release and overnight stop rules

Qualify one joined frozen bundle, including the already integrated lookup index
and matching Harness pin. Record cold setup, warm notebook, lookup, unchanged
reload, changed reload and recovery timings separately. Confirm unchanged reload
with no extra checks triggers no compiler preparation; the type-identity repair
must not add another GHC pass or discard retained support. Compare warm behavior
with retained wave evidence, without blocking delivery on unrelated tuning.

Run a bounded Sol 6.1 medium real-provider smoke through the same bundle: read a
memory block, edit an ordinary helper, reload, use it, and verify a no-op reload.
Observe actual normalized provider usage/affinity; report insufficient cache
evidence rather than repeatedly sending calls merely to obtain a hit.

After production gates pass, merge the complete reviewed result into unified main,
push the approved repositories/pins, record a durable qualification tag/receipt,
and launch Astra-medium Persona with its authored files and memory preserved.
Old compiled runs remain bound to their old binary. Retain replacement readiness,
URL, source/bundle hashes, checks, timings and known limits in the delivery handoff.

Aim to spend the overnight window on the critical dependency chain: shared
identity contract, compiler/native integration, hosted history, matched delivery.
Once the first joined hosted history passes, stop adding improvements. Fix only
release failures and scoped review findings. Log unrelated discoveries with their
evidence for later. If a design gate fails, minimize the counterexample and use
the coordinator to revise that boundary; do not silently weaken the contract or
substitute a simpler workload. No time estimate constitutes release evidence.
