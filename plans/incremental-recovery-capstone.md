# Incremental recovery capstone

## Outcome and delivery order

Finish the current correctness and performance work and deliver a qualified
human-testable engine/Harness. The immediate baseline is the reviewed schema-11
recovery codec joined at `bfe3822cde`. Its component evidence is 69 passing checks;
joined release qualification remains required. Qualify and preserve that bundle
while the structural replacement below is developed. Do not transfer its
acceptance to the later format.

The structural release replaces cumulative proof hashing and complete recovery
manifest publication. It does not add a parallel persistence framework. Keep
one compiler evidence owner and one runtime transaction owner. Every parcel
compiles affected production and test consumers and executes focused tests
before integration. Root owns integration and matched bundle qualification.
Use the provisioned independent checkouts and current admitted parallel build
policy in `docs/swarm-builds.md`.

## Compiler evidence contract

The toolchain issues immutable **binding-scope proofs**. A v3 scope identifier
hashes the newly bound source nodes and their complete labeled outgoing rows,
including zero-edge nodes. Typed local references are normalized for hashing;
external references retain their exact earlier scope identifiers. Unrelated
inherited rows are excluded. An admission with no new bindings issues no proof.
Already bound outgoing rows cannot change. Artifact cycles inside one issued
scope remain valid. Scope ancestry points to older immutable scopes.

A native group can belong to a different scope from its artifact carrier.
Never construct an artifact occurrence from a group's scope alone. Proof
retention is independent of selected payload/native custody: unselected witness
rows neither require available payloads nor authorize them.

Keep these identities distinct in the types:

- The immutable binding-scope identity.
- A durable reference to an exact context and its available locator.
- A semantic observation digest of the selected context.

An exact context carries the compiler namespace, typed artifact and group roots,
native custody, compiler roles, and lexical facts. Store it compositionally as
one exact predecessor plus typed additions, removals and replacements, including
facts imported from another branch. Changes originate at the issuing owner;
the persistence layer must not recover them by diffing cumulative snapshots.
Explicit empty selections stay empty. Do not union ambient inventory.

Use existing persistent resident maps and iterative cold reconstruction with
shared validated prefixes. A growing predecessor chain must not become recursive
stack growth or repeated quadratic replay. Batch immutable evidence through the
existing materialization owner; do not create a file per graph vertex. A held
durable materialization receipt can be reused under its owning lifetime; a fresh
acquisition or cold recovery still authenticates its input. Disk facts alone
never mint source-compilation-instance authority.

V3 intentionally changes identity semantics. Update every affected evidence,
production-entry and prepared container format and observation domain together.
Fresh runs use the new format; old runs retain their matching old readers.
Regenerable caches may rebuild after explicit format refusal.

## Cold validation

Validate complete labeled outgoing rows against authenticated certified
requirements once per selected source. Preserve original/canonical carriers,
interface seals, producers, full native labels, exact owner keys and ordinals.
Never resolve an edge through an available-owner inventory.

Preserve the existing per-source transitive conflict rule without a traversal
for every source: group selected payloads by the existing interface
`ExactModuleIdentity` and native `NativeOwnerKey` keys. Only keys with distinct
competing payload IDs need reverse reachability. Seed every selected typed
occurrence (artifact and group), include the seed, and traverse every dependency
kind. Refuse when an ancestor reaches competing versions. Equal payloads in
different scopes are not conflicts; disconnected conflicting versions remain
valid when no ancestor reaches both. Namespace uniqueness and root/custody
closure remain separate checks. Unselected proof rows supply only structural
and digest evidence.

Retain the slow transitive algorithm as a small-graph independent property oracle.
The unique-owner case avoids repeated searches; report competing-version work
honestly rather than claiming universal linear complexity.

## Runtime transaction contract

One SQLite database per existing recovery domain replaces authoritative JSON
manifest rewrites. SQLite owns its WAL, transactions and indexes. Every writer
connection uses fixed WAL/FULL durability. Checkpoint maintenance cannot turn a
successful commit into a failed semantic operation.

Store immutable declaration records and exact-context references, changed
binding winners/tombstones, source-instance facts, actor owners, public epochs,
the store revision and the durable allocation ceiling. Preserve full `u64`
values without signed narrowing, using an exact representation such as
eight-byte big-endian blobs. Do not hide cumulative JSON snapshots in SQL rows.

Publish raw artifacts and immutable evidence durably before committing their
references. Retain and account for abandoned staged objects; there is no new
online collector or publication-time deletion in this work.

The issuer prepares `PreparedRecoveryDelta` from changed facts, an exact base
owner/revision and already prepared resident changes. The store exposes typed
not-committed, committed, visible-but-durability-unconfirmed and outcome-unknown
dispositions. Unknown/unconfirmed dispositions fence further publication and
allocation until reconciled. Do not replay compiler work or effects.

After successful COMMIT, install preprepared live state without an ordinary
fallible operation or checkpoint between commit and activation. A later cleanup
or activation problem must preserve the committed disposition. Compare the
relevant exact owner/public epoch/store revision; stale staging can retry held
facts without rerunning effects.

## Generation range leases

An opaque non-cloneable lease belongs to one SessionLib/store/admission owner.
Keep durable reserved ceiling `C` separate from consumed generation `U`;
`DeclLog::generation()` and `next_module()` retain consumed-cursor semantics.
Prepare only the requested reserved slots. For request `n`, checked-add `U+n`;
if it exceeds `C`, durably extend by at least the shortage and normally 64 IDs,
capped at the remaining `u64` space. Requests larger than 64 and a final partial
range are valid. Consume a confirmed tail at `u64::MAX`, then return typed
exhaustion. Remove unchecked prediction arithmetic.

Consumption inside a confirmed range writes no durable metadata. Restart sets
the consumed floor to the durable ceiling, burning the unused tail. Ordinary
publication cannot lower the ceiling to the live cursor. Unconfirmed visible
reservations expose no IDs and fence use pending confirmation; unknown outcomes
also fence. A child inherits a conservative durable floor, never its parent's
lease. Imported floors are installed before local issuance. An unused lease may
be discarded and raised; a floor change cannot repair already exposed collisions.

## Harness and test tooling

On the compatible Harness lineage, establish fixed FULL writer durability and
an admission barrier for stores previously written with NORMAL. Remove the
per-binding full checkpoint and mode restoration from successful acknowledgement.
Keep exact retry authorization and typed committed outcomes. Measure ordinary
write impact. Pin only the reviewed compatible commit; canonical Harness main
has independent unjoined API changes and is not an interchangeable ancestor.

Repair property runner source/persistence identity through the owning runner.
Prove a deliberately failing case writes a replayable seed through the declared
native command, then replay it. Absence of a warning alone is insufficient.

## Acceptance and bounded completion

Use production issuers and independently expressed models. Cover graph cycles,
diamonds, sharing, shadowing, group/carrier scope differences, explicit empty
contexts, cross-branch histories, retirement and cold reopening. Mutate valid
facts to prove refusal sensitivity. Cover fault schedules around evidence
durability, transaction commit and activation; cancellation, stale and sibling
publication; and range no-reuse, overflow and restart histories.

Record bytes encoded, hashed, validated, read and written and rows/edges visited
separately. Small publication/reservation work must be bounded by changed facts
rather than retained history. Measure cold reconstruction independently.

The matched release requires the current 29 correctness cases, the one-child
preflight before prepared-child acceptance, catalog acceptance, affected prepared
corpus checks, both Harness performance workloads and the staged real Sol root,
two Luna children and one Luna grandchild exercise. Use the central frozen bundle
owner for exact source/profile/compiler/assets, counts, deadlines and cleanup.
Finish the existing paired fast-dev/production performance campaign in
`plans/compiler-harness-reuse.md`; separate queue, service, wall, allocation and
retained RSS. Parallel correctness timing is shared-load evidence.

Deliver the baseline when qualified; qualify and annotate the final structural
release independently. Remove obsolete writers, cumulative proof encoders,
duplicate test setup and superseded plan material on final integration. Additional
work in this capstone requires a concrete failure, required integration repair or
measured regression; retain unrelated audit leads without extending the gate.
