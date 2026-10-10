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

Production roots come only from the pinned owning materializer or authenticated
matched catalog producer. Verify content, library shape and exact semantic
facts/custody. Do not rebuild canonical HAMTs during ordinary cold recovery.
Canonical rebuild and independent lookup/enumeration checks belong to properties
and qualification. This work does not add arbitrary external-root admission.

Catalog evidence uses the same blocks/encoding in a closed self-contained
read-only SQLite block image. Verify/import the reachable closure once into run
blocks; children share roots thereafter. Keep raw GHC/native file materialization
under its existing owner. Publish the image in DELETE journal format; never
copy only a live WAL database's main file.

Bump affected recovery, exact evidence and catalog contracts together through
their owning writers. Refuse incompatible state before writer PRAGMAs. Old runs
keep matching old binaries; no migration or dual writers. Regenerable caches may
rebuild after explicit format refusal.

## Delivery and acceptance

Use separate provisioned worktrees. Each implementation owner compiles changed
production and test consumers and executes focused counted checks before
integration. Root integrates and verifies cross-boundary behavior. No build-owner
permission queue: parallelize against measured host/cgroup headroom.

Parallel owners: exact identity/scope; persistent snapshots/materializer;
run database/publication; leases and recovery consumers; catalog deployment;
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
