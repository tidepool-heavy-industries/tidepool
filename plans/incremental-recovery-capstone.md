# Crate-backed recovery and M2/M3 completion

## Outcome and accepted decisions

Replace bespoke snapshot journals and cumulative recovery manifests with library
persistent maps and one run-owned SQLite store. Preserve exact compiler authority,
independent captures, atomic publication and completed-effect receipts. The frozen
36f82f312668 baseline executed 29 checks: 14 passed and 15 failed; it is unqualified.
The retained baseline is evidence, not acceptance for the replacement.

Accepted simplifications: one database per run with independent machine-session
domains; immutable blocks retained until run retirement; content-addressed snapshot
roots instead of predecessor replay; historical contexts opened on demand; SQLite
constraints and RAII transactions; a fresh-format cutover. Unknown commit outcomes
fence the run store and require authenticated reopen, without effect replay.

## Dependencies and encoding contract

Use exact direct pins imbl 7.0.2 (no optional features), fvm_ipld_hamt 0.10.6,
fvm_ipld_blockstore 0.3.2, fvm_ipld_encoding 0.5.4, cid 0.11.3 (std and serde),
and multihash-codetable 0.2.2 (blake2b, no default features). The compatible lock
uses serde_ipld_dagcbor 0.6.4, not 0.7. Keep existing petgraph and proptest.

Upgrade Tidepool and the matched Harness together to rusqlite 0.40.2, using
bundled, cache and fallible_uint with defaults disabled; lock libsqlite3-sys
0.38.2. This published bundle contains SQLite 3.53.2, including the WAL-reset
fix missing from the old 3.46.0. It is not the latest SQLite release. Verify
sqlite_version() and sqlite_source_id() from actual Buck-linked binaries.
Regenerate Cargo and Buck through their owners and retain dependency notices.

Fix durable maps to HAMT V3 with bit_width=5, min_data_depth=0,
max_array_width=3. Key routing is SHA-256; block identity is CIDv1,
DAG-CBOR codec 0x71 and BLAKE2b-256 multihash 0xb220. Scope IDs and compiler
artifact identities keep their own explicitly distinct hashing contracts.
Encode compound keys as stable typed tuples into BytesKey; never use rendered
names or complex-key CBOR maps. Ordered adjacency uses sorted tuple arrays.

## Shared ownership and integration contracts

The toolchain owns exact semantic facts, scope proofs, immutable snapshot maps,
typed SelectionRoot and ContextRoot references, FVM materialization and semantic
rehydration. The runtime owns run storage, domain identity, publication,
allocation, public surface facts and host recovery. Toolchain must not depend on
runtime. Use upstream Blockstore at the storage boundary; no additional generic
persistence framework or second independently updated model.

The materializer stages immutable blocks and returns their typed root references
while retaining their bytes. The runtime commits blocks and roots in its existing
publication owner; only successful commit issues a durable receipt. A staged or
decoded CID is neither a durable receipt nor compiler authority. Exact compiler
reissuance continues through authenticated artifact custody and existing owners.

Resident exact facts use imbl::OrdMap with immutable, compact Eq values.
Keep the actual shared base and next snapshots; consume their structural diff
to update operation-local FVM HAMTs. No collecting whole maps to reconstruct
changes and no separately recorded mutation journal. Short-lived HAMTs do not
cross shared Send/Sync boundaries; shared resident receipts retain imbl maps and
immutable facts. General-purpose persisted maps must serve both compiler and
runtime snapshot consumers through this one implementation.

Large proofs, roles and lexical facts are CID-valued leaves; selected roots and
custody sets use compact identities/unit values. Published-source roles reference
a graph-only SelectionRoot, never their containing full ContextRoot. Preserve
the distinction between whole-context content identity and the semantic
selection observation used by compilation. Proof-only ancestors do not affect
an observation of an otherwise unchanged selected context.

## Exact compiler meaning

V3 scope identity hashes every newly bound node and its complete labeled
adjacency, including zero-edge nodes. Normalize typed local endpoints for
hashing; external endpoints retain their exact previous scope IDs. Existing
outgoing rows are immutable. No new binding means no new scope proof.
Semantic cycles inside one scope remain typed references, not CID cycles.

Carry issuer-selected NativeGroupBinding through admission inheritance,
selection, support extraction, merge and recovery. A group has an independent
issuing scope; its explicit carrier edge may reference an older artifact scope.
Never reconstruct a selected group from carrier plus ordinal or pick an
arbitrary match. Preserve ambiguity refusal when input is genuinely unqualified.

Proof availability, selected payload custody, namespace, compiler roles and
executable demand remain distinct. An unselected proof row need not have an
available payload and grants none. Retain the reviewed direct-row cold validator
and its independent old traversal oracle, including exact per-source conflict
semantics.

## Run storage and publication

Use four SQL responsibilities only:
- store_meta: database identity and format/encoding contract;
- blocks: immutable authenticated CID-to-bytes;
- recovery_domains: domain identity, lineage, revision, content revision,
  durable allocation ceiling and domain snapshot root;
- public_owners: domain plus public owner, epoch and immutable surface root.

Declarations live once as immutable blocks addressed by a domain declaration-map
root. Surface blocks reference declaration tip, compiler context, binding-winner
map and source-instance map. Public owner/epoch stay outside immutable surface
content so authorized owner transfer can preserve the content root. Delete
duplicate SQL artifacts, edges, winners, source and staging tables: they have no
independent query consumer.

A stable root-domain identity survives root host reincarnation. Fresh independent
machines use the existing SessionId issuer and refuse collisions; actors sharing
a machine share its domain. Qualify every mutable SQL key/CAS by domain.
Database location, canonical host run root and exact compiler artifact directory
are distinct inputs. Preserve child directory and held host lease checks.
Startup tests domain absence, not database-file absence.

One run-owned store serializes its short write transactions; no compiler work,
evidence construction or long-lived capture runs inside a transaction.
Use TransactionBehavior::Immediate, prepared statements and SQLite constraints.
Full-u64 scalars use big-endian eight-byte BLOBs with strict storage-class/length
checks. Prepare artifacts durably, encoded blocks and next resident state before
the transaction. Commit immutable inserts, domain root/revision and changed
public owner roots atomically with expected revision/epoch checks.

Publication returns Committed, Refused or RecoveryRequired. Known refusal before
commit requires confirmed transaction closure. Any COMMIT error or failed
rollback conservatively fences the store and invalidates outstanding publication
tickets/leases until authenticated reopen. Do not use DropBehavior::Commit or
infer durability from a revision SELECT. Successful WAL/FULL commit immediately
installs already prepared resident state, without a fallible checkpoint or
materialization step. Checkpointing is maintenance.

Keep consumed generation U distinct from durable reserved ceiling C. Reserve
normally 64 IDs before exposure; support larger requests and the last partial
range with checked full-u64 arithmetic. In-range consumption writes no metadata.
Reopen starts at C, burning the unused tail. Children inherit a conservative
floor, not a parent's lease. Install imported floors before local issuance.
Publication never lowers C to U.

## Recovery, retention and catalogs

Keep immutable blocks for the run lifetime and account disk/orphan usage; no
online collector or per-block refcount protocol. Native/mutable resources retain
their existing lifetime owners. Verify CID bytes on fresh reads/keyed writes and
retain verification with the owning immutable receipt. Distinguish missing
blocks, storage errors, malformed encoding and integrity refusal.

Hydrate active/public selected closures and requested captures. Historical
declarations retain typed roots rather than eagerly expanded contexts. Shared
block/proof observations are reused in one recovery acquisition; no long-lived
SQL read transaction belongs to a capture.

Production snapshot roots come only from the pinned owning materializer. Verify
content, library shape and exact semantic
facts/custody. Do not rebuild canonical HAMTs during ordinary cold recovery.
Canonical rebuild and independent lookup/enumeration checks belong to properties
and qualification. This work does not add arbitrary external-root admission.

Deployment catalogs retain original products, interfaces and their authenticated
compiler receipts through the existing catalog owner. They contain no runtime
context roots and need no second SQLite block image or import protocol. Original
entry dependency records contain only equality observations of issued interface
and reusable-original roles plus the native selection digest. Reopening reissues
the selection from authenticated originals and compares those observations;
decoding them grants no compiler authority. Published-source declaration policy
belongs to the run context owner and cannot occur in these entry records.

Bump changed recovery and exact evidence contracts through their owning writers.
Keep unchanged catalog/entry wire contracts: narrowing entry roles preserves the
two variants their producer issues and refuses unissued runtime-context policy.
Refuse incompatible state before writer PRAGMAs. Old runs
keep matching old binaries; no migration or dual writers. Regenerable caches may
rebuild after explicit format refusal.

## Delivery and acceptance

Use separate provisioned worktrees. Each implementation owner compiles changed
production and test consumers and executes focused counted checks before
integration. Root integrates and verifies cross-boundary behavior. No build-owner
permission queue: parallelize against measured host/cgroup headroom.

Parallel owners: exact identity/scope; persistent snapshots/materializer;
run database/publication; leases and recovery consumers; entry observations;
Harness/SQLite dependencies; properties and diagnostics. Publish concrete shared
interfaces early. Preserve old WIP branches while deleting obsolete machinery
from the integrated implementation.

Use independent BTreeMap/recomputed graph oracles, small key domains, branching,
replacement, deletion/reinsertion, shared-tree boundaries, cold reopen and
equivalent histories. Compare incremental roots to canonical rebuilds and check
lookup/enumeration agreement. Exercise actual issuer-to-consumer paths for
duplicate carrier/ordinal groups, custody, narrow/merge/capture and recovery.

Inject refusal and crash schedules around artifact durability, commit and
activation. Check stale and sibling publication, owner transfer, cancellation,
generation no-reuse/exhaustion, missing/corrupt blocks, format refusal and catalog
relocation. Repair/reuse the qualified property persistence runner. Production
fixtures come from current issuers, not handwritten blobs or authority factories.

Measure encoded/hashed/read/written bytes, blocks/SQL work, allocation and wall
time separately over growing histories. Unchanged snapshots emit no new blocks;
confirmed range consumption emits no SQL; sparse edits do not serialize history.
imbl diff cost follows unshared structure: cold rebuilt or broadly edited/undone
maps can require linear work, which must be reported honestly.

Require all 29 baseline cases, one-child preflight/prepared-child acceptance,
affected corpus checks, both Harness perf workloads, and the short real Sol-root,
two Luna-child and Luna-grandchild exercise with complete cleanup. Diagnose the
remaining timeout/assertion failures rather than presuming storage repairs fix
them. Qualify one exact frozen source/profile/compiler/assets bundle. Preserve
existing human sessions until replacement qualification. Merge/push qualified
work and create an annotated tag binding exact revisions, bundle and evidence.

Completion deletes cumulative manifests/witness encoders, custom issued-delta
chains and replay, duplicated SQL fact hydration, online commit reconciliation
and obsolete caller/test paths. Replace superseded plan material. Additional work
requires a concrete failure, integration repair or measured regression.

## Current implementation wave: immutable owners and bounded publication

The delivery branch starts this wave at `4c36f6bf2b72`. It is not qualified.
The sampled admission interval attributes about 85% of CPU to inventory
installation dominated by artifact comparisons. Finish the following contracts
and the existing pending integration repairs before freezing another candidate.

### Complete artifact facts, separate issuing authority

The existing ArtifactInventory owns canonicalization. Represent immutable facts
as an enum of typed Arcs: OriginalFacts, CanonicalFacts and JoinedFacts. Do not
wrap authority-bearing certified facades recursively inside these facts.
OriginalFacts requires a canonical companion and authenticated native witness;
incomplete construction is a private draft. JoinedFacts includes its complete
normalized requirements and role before issuance, including recovered joins.
CanonicalFacts contains immutable receipt, producer, interface/package bytes,
optional Core, origin and home-unit facts. Execution graphs remain optional only
when their original certification permits absence.

Fresh source permission and catalog custody belong to issuing/view facades.
Absence explicitly withholds authority and cannot fall back to an inherited
grant. Splitting only ArtifactEntry is insufficient: certified original and
canonical owners must share the immutable facts as well. Canonical projections
borrow facts or return a cheap certified facade with the receiving authority.

Authenticate independent ingress, compare its complete facts once against any
same-ID resident owner, and return the inventory's canonical handle. A known
handle takes a pointer fast path. ArtifactId alone is insufficient because
joined requirements are not sealed by it. Keep structural comparison private
to ingress; remove deep equality from per-group installation and ordinary views.
Adopt the complete original owner: never replace its individual byte Arcs while
retaining a witness anchored to the old bytes. The finalizer establishes that
relationship once. Native census, adjacency and owner indexes belong to the
authenticated witness and are borrowed by consumers.

ArtifactView remains the live leased capability. Decoded ExactArtifactSelection
is inert transport and becomes a live view only through authenticated recovery.
Installed graph outgoing rows are immutable. A consuming binding transaction
validates a complete insertion before mutation; reclamation removes only unowned
rows. Drop the graph guard before destructing leases. Preserve read-only petgraph
traversal, cold checks and conflicts against independently submitted rows; remove
whole-parent witness revalidation on each valid live extension. Defect injection
belongs at raw ingress or the graph owner, not in an otherwise valid live view.

### Incremental segment admission

Capture produced interfaces once per physical request, retaining bytes,
certification and item ordinal. BoundSegment retains the compiler's fixed native
owner selection, exact issued bindings and produced-output inventory. The
existing inventory is the only namespace/binding resolver.

One SegmentCursor owns admitted prefix and accumulated demand. Item admission
borrows it mutably, commits prefix and demand together on success, and leaves
semantic state unchanged on failure. Issue a missing exact group closure once,
including mutually dependent new groups as one installation. Reuse old group
handles without namespace reselection. Do not eagerly bind all native groups or
use carrier ArtifactId root selection, which broadens group demand. Preserve
historical dependency custody and distinct group/carrier scopes.

Current/prior output type evidence follows item ordinal; runtime value authority
requires completed prior items. Future output interfaces remain private. Sparse
generations, discard slots and declaration barriers retain their semantics.
Final captures add contextual authority using the already captured certificate.
Borrows stay inside a synchronous phase; suspended captures own narrow views.

### Publication state and semantic currency

Use OwnerStamp (required owner Arc plus epoch), affine JoinReservation,
PublicationSemanticBase, DurableWriteBase, CertifiedPublication and
StagedPublicManifest. Independent optional capabilities remain optional;
declaration payload versus binding-only payload and durable versus ephemeral
state are sum types. A declaration payload owns its reservation and receipt.

PublicationAttempt distinguishes Settled(PublicManifestCommit),
Rebase(FinalExecutionIntent), and RestageMetadata(CertifiedPublication). Terminal
PublicManifestCommit has no Stale case. Relevant public changes require a new
join against the new semantic base, retaining private declarations and effects.
Storage-only changes reuse the same certificate and reservation. Capture a new
coherent DurableWriteBase and apply the delta to its latest complete graph;
never replace unrelated private or other-public-owner work with an old candidate.
Invalid private intent or owner epoch is terminal. Post-claim refusal and unknown
commit remain terminal/fenced, never retryable execution.

Reservation consumes a generation immediately; dropping it burns that identity
without asynchronous rollback. Reservation, burn and ceiling changes do not
advance declaration-fact revision or invalidate semantic compiler views.
Committed/restored/activated declaration facts do. Paired baseline v3 hashes
library/session/owner identity, relevant public/private visibility and existing
scoped compile-view commitments, excluding global log revision, SQL revision and
allocation ceiling. Keep TPDJOIN and receipt version 3 and lexical-projection
identity unchanged. Keep full typed comparisons at admission; hashes do not grant
authority. RecoveryGraphCandidate derives revision/ceiling from DurableWriteBase.

Migrate ordinary/final private publication and activation-input publication
together. The existing exclusive session borrow performs preflight, short claim
and commit. Expensive staging, serialization and compiler work happen outside
that critical section. Metadata restaging never registers compiler work.

### Compiler lease and host cancellation

Use one pipeline: prepare/certify, complete existing declaration projections,
close the compiler transaction, seal host results, publish. First-pass target
admission remains before projection issuance because declaration certificates
require it. Preserve malformed receipt/binder/slot refusals before projections.
PreparedCellProgram privately owns checked items, certificates, immutable output
custody, pending publications and context, with no compiler endpoint.

Exit CompilerWorkTicket::run_with_close normally after the final projection.
Then consume the prepared result for prefix construction and contextual capture.
Keep one implementation for actor, fixture and performance callers. Successful
physical request counts do not increase. Do not introduce an early-finish TLS
escape hatch or release/reacquire the worker mid-transaction.

Logical host cancellation outlives compiler cancellation registration and is
passed explicitly into post-close work. Completed output and confirmed close
are distinct outcomes. An unconfirmed close retains required custody and never
authorizes replay. Retain TempDir/file owners until bytes or immutable retained
outputs have transferred; a scratch path alone is insufficient ownership.

### Implementation, work counters and qualification

Sol owns facts/inventory, publication and actor callers, segment admission,
compiler lifetime, Harness completion, and typed observers/property coverage.
Luna owns bounded build/fixture/report work with semantic review where needed.
Publish buildable interfaces before dependent parcels. Root owns integration;
each isolated worker compiles production and test consumers and executes its
focused checks independently. Initial admission is three heavy hosted cases
total with roughly 20 GiB host reserve, adjusted from actual peaks.

Finish existing build-input/C-driver/header/binding-owner repairs, Harness
forms/cancellation/GUI integration, browser command joining and narrowed recovery
observers. Preserve the human server on port 8081 and all source WIP. New findings
require a concrete mechanism and a bounded neighboring search, not an open-ended
new audit wave before delivery.

Extend existing bounded observations with canonical hits/comparison calls and
bytes offered, issued/reused groups, output certifications, physical requests,
semantic rebases, metadata restages, worker residence and host phase timings.
Do not add per-hit logging. Deduplicate physical requests by their real identity;
inclusive phase times cannot be added as independent costs.

Required work contracts: known canonical reuse compares no payload bytes;
each produced output is certified once; old exact groups are not reissued;
metadata restaging makes no compiler request; unrelated history is not walked
for every admission; post-compiler sealing holds no physical compiler lease.
Exercise independent equal allocations, authority absence/overrides, lexical
requirement conflicts, graph cycles/sharing/recovery, sparse output histories,
and fault schedules with independent models and shrinking. Calibrate targeted
mutations and retain deterministic regressions. Reuse native compile-fail
infrastructure with positive controls; do not expose private authority factories.

Publication controls include reserveA/reserveB before certification, actual lease
refill, unrelated private C preserved after reopen, separate public owners,
semantic change, cancellation before/after claim, retirement and commit ambiguity.
Execute an actual counting effect once through the owned workbench while retries
occur. Progress models use bounded interference followed by fair drain.

Finally freeze one matched production bundle and execute every required central
cohort, including M1/M2/M3, result delivery, child preparation and both harness
performance scenarios. Compare three matched repetitions of repeated A,A,B,A,
retained reuse and scripted root/child/grandchild scenarios, reporting cold/warm,
compiler/host/queue time, allocation and retained RSS separately. Run a short
real Sol 6.1 root/two Luna children/grandchild trial in a fresh workspace, then
shut that trial down. Update main and annotate exact acceptance with source,
Harness pin, bundle descriptor and evidence identities; old component passes
are not acceptance of the new bundle.
