# Cross-cutting G2 contracts

These constraints replace the earlier v12 supplement. That supplement permitted
a manifest-less segment item and a bare Inherited marker while requiring exact
roots downstream. Do not preserve those contradictions for compatibility.
Concrete names below describe obligations, not required public API spelling.
The 5c759554 review keeps these distinctions and refines their implementation in
[PRIVATE-STATE](PRIVATE-STATE.md) and [PACKAGE-CLOSEOUT](PACKAGE-CLOSEOUT.md).
[NEXT-WAVE](NEXT-WAVE.md) owns current assignments and sequencing.

## 1. Program and selected imports travel together

The producer owns a program's package environment while projecting that program.
Publication must carry the program and its exact ordered roots as one paired
value. This applies to ordinary targets, checked World items, and genuinely
fresh HOME products. Retained HOME imports pass through their existing exact
completion; they are not reselected as fresh.

| Identity | Meaning |
|---|---|
| Envelope SHA | Immutable encoded payload bytes, shareable across occurrences |
| Certified product SHA | Envelope plus ordered interface commitments; not exact child history |
| ArtifactId | Existing descriptor identity; neither occurrence nor permission |
| Projection/Occurrence coordinate | Selection inside one authenticated producer output |
| ArtifactBinding / PackageCodeBinding | Existing owner-issued exact binding and ordered dependency history |

A distinct occurrence may contain byte-identical row fields. Unique occurrence
IDs/ranges and authenticated payloads are required; byte inequality between
occurrences is not. Do not introduce recursive graph hashes.

### Producer shape

Use one immutable stage/output occurrence table and exact roots for each actual
consumer. An item carries its selected roots, or an authenticated owner-scoped
reference to them, while still paired with its original item identity/program.
Do not duplicate the full graph into every item. The parent stage identity,
checked item identity and prepared program import order must be tied at the
existing certificate boundary; an unscoped numeric index is not authority.

Capture HOME roots from the actual successfully issued fresh product/group.
Do not project every group's body again for each target to reconstruct imports,
or use a union of target symbol maps. A genuinely shared HOME issuer supplies
one environment reused by its consumers. Separate environments remain separate;
matching bytes or bisimulation does not establish common issuance.

If an emitted fresh HOME product requires additional package demand, feed that
exact demand through the existing package worklist once. The final target's
closure alone does not establish completeness for every HOME body. Unemitted,
unsupported or unrelated bodies must not become compulsory dependencies.

### Rust shape

Decode and authenticate into an opaque bound package selection associated with
its actual program and issuing output. Admission consumes that paired value,
not raw PackageLinks plus optional target name. Ordinary and checked-item
constructors must both exist. Empty demand is a verified empty selection,
not None. Missing required roots refuse before graph issuance.

Retain occurrence coordinates while selecting the demanded finite closure.
Only then translate into the existing IssuedPackageCodeGraph / ScopeProofV3
owner. Do not submit all unrelated output occurrences as one ArtifactId-keyed
plan. Separate independently selected environments have separate issuance
scopes; immutable payload storage remains shared.

Preserve ConflictingPlan for genuinely conflicting definitions within the SAME
issuance scope. If a real single selected projection needs two histories of
one ArtifactId, produce a minimal real counterexample and extend that existing
owner's coordinate deliberately. Do not solve it by overwriting a row, changing
the product hash, or inventing another graph issuer.

### Fresh versus retained

An already completed HOME row reuses its exact binding and imports. If the new
receipt also carries fresh evidence for it, validate that evidence explicitly
against the row's owner/selection; do not mix validation with a cursor advanced
only on fresh issuance. A private enum or paired branch input should prevent
a prior-completion path from leaving a fresh slot iterator unconsumed.

Newly projected package tables should contain total Local edges: the existing
demand worklist is expected to close their required package products. Delete
the bare Inherited marker and seal/symbol search if this is established for all
emitted ordinary/item/fresh-HOME producers.

If a concrete producer requires a held terminal, stop that deletion and present
its exact counterexample. An acceptable terminal carries an exact coordinate
issued by the input owner and authenticated against that input, never a marker
resolved from all retained graphs. Existing retained HOME edges already bypass
the fresh table; preserve that route.

### Wire and refusal

P freezes one versioned producer/reader contract after reviewing all three
receipt variants. A new internal version is authorized. Reject older layouts
that lack required choices; no compatibility reconstruction. Do not alter
TPGRP payload hashing or already-issued durable graphs just because a separate
linkage receipt changes.

At admission authenticate output/stage/item scope, producer/profile, payload
hash, owner/interface/version, ordered import count, symbol/signature/
representation/evaluatedness, and every reference range. Preserve errors as
typed distinctions. Byte-identical rows with distinct IDs are legal. Missing,
foreign, crossed-item and out-of-range roots are not.

Delete the producer partition-refinement canonicalizer and decoder identical-row
prohibition together with the HOME union resolver they were compensating for.
This deletion is coupled to correct per-consumer scoping, not a standalone
relaxation. Payload byte deduplication stays.

## 2. Historical custody never chooses compiler roles

Selected interface, selected Original, retained native availability and executable
demand remain distinct. RetainedCore deliberately lacks an Original namespace
entry. A metadata inventory does not authorize promoting it.

Use the existing selected source/output issuer and exact native edges. Preserve
needed carrier/dependency bytes through narrowing even when their historical
versions are not selected in the compiler namespace. For a genuinely selected
Original, traverse THAT binding's children; adding an unused same-interface
Original cannot change the result. No owner-keyed last/first match.

A source role change, if actually necessary, must come from the input/output
issuer that knows the selected carrier and canonical pair. Do not choose from
metadata.artifacts or all availability offers. Keep partial group permissions
and namespace-only dependencies separate from exact native execution edges.

## 3. Interface reuse is an authenticated owner decision

Same owner/seal with different capture paths can be relocation of selected bytes.
Authenticate the observation and retain the existing selected witness/token on
reuse. New additions must not override it through catalog precedence.

Use this decision on both segment and ordinary routes, preserving their input
semantics: a duplicate segment-item delta is not the ordinary full receipt's
legitimate overlap with inherited selection. Do not weaken global
PackageInterfaceWitness Eq, normalize paths into authority, or infer absence
from failed reads. Retained immutable-token use may outlive the old file;
legacy path-only evidence must still be read and may refuse.

## 4. Private publication commits one coherent state

The existing execution/publication owner tracks original admitted state,
private completed progress and latest public state. It must prepare one paired
runtime source selection / compiler World / visible binding transaction.

For truly unchanged work preserve latest public progress exactly. Compose real
private changes from owner-issued deltas under existing fences. Public-only
selections are validated against public custody; private transfers against
their exact admitted/completed evidence and prepared transfer. Do not replace
latest state with the private snapshot, union arbitrary inventories, or bypass
a conflict because the resulting names look correct.

Record started/pending/completed/refused progress where that transition occurs.
A freeze-time comparison cannot establish no work started. Keep immutable birth
authority separate from the current preparation snapshot. One authored program
owns advancement across its items and parked effects; preparation alone grants
no advancement ownership.

A visible retained host result carries real binding/type evidence. Synchronous
acceptance after the existing cancellation fence records its exact delta; at a
quiescent boundary, before new authored preparation or freeze, the session owner
applies that delta to the current compiler/runtime state. Do not mutate the
predecessor beneath an already-running checked program or pretend accepted means
already applied. Neither host progress nor declaration-only progress is a fabricated
zero-cell execution. Hidden operands and activation inputs retain their distinct
authority. No second publication path or caller-provided success boolean.

Keep cancellation claim order, same prepared promotion witness, durable commit,
original effect receipts, cleanup uncertainty, and recovery-required outcomes.
Failure after a handler ran never authorizes replaying the handler.

## 5. Evidence and deletion

Positive tests enter the actual production issuer/consumer. Raw transport
mutation is useful for refusal controls, not minting authority. Assert the
adversarial premise before asserting the outcome. Check ordered edges and exact
bindings; nonempty lists, owner unions and depth-limited flattening are not
history oracles.

Current compiler qualification uses declared fixture producers. The existing
captured CBOR/HI pair may be retained as a narrowly documented frozen-reader
regression with validated provenance/format metadata; it cannot stand in for
current output issuance. Preserve the counterexample while changing ownership.

Logging uses the existing JSON/tracing owners and a typed decoder. It cannot
change compiler outcome or swallow cancellation. Keep timing/END/reuse protocol
consumers separate. Delete obsolete parsers, selectors and tests of removed
behavior, while retaining meaningful failure cases.

No new provider, scheduler, event framework, cache tier, full-package compiler
path or authority factory belongs to this repair. Escalate only a minimized
concrete contradiction in these contracts; keep the independent frontier moving.
