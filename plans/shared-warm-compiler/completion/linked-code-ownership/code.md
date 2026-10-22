# One demanded linked-code graph

B owns the pure immutable graph in `tidepool-repr` and the acquisition/target
issuer in `tidepool-toolchain`. Codegen, runtime and snapshots consume that same
graph; none defines a parallel projection. Root owns the codegen bridge; A/D own
runtime and snapshots. Coordinate APIs directly, copying root on contract changes.

Replace the executable portions of `IssuedNativeSelectionCompletion`,
`CompletedNativeSelection` and `IssuedPackageCodeGraph`; do not layer a new graph
over their permanent reconstruction paths.

## Concrete interface

- `NativeCodeComponent`: immutable dependency SCC; owns authenticated definitions,
  canonical ordered local edges, external child references and retained requirements.
- `NativeCodeRef`: opaque component owner plus canonical member reference.
- `LinkedNativeTarget`: checked target/entry and exact ordered demanded references.
- `AcquiredNativeCode`: code reference paired with existing acquisition/source
  authorization. Equal code IDs cannot substitute a foreign requirement/grant.
- Pure code-ID bytes are observations, not capabilities. Only the issuer can
  construct usable graph references or acquired code. Keep nominal HOME/PACKAGE
  body cases and their existing certificate requirements exhaustive.

## Identity

HOME labels preserve exact CachedHomeOwner (unit, module, ModuleVersion, canonical
interface/product identity), original ordinal, definitions and export contract.
PACKAGE labels preserve authenticated PackageCodeOwner, canonical body and exports;
package ordinal normalization remains owned by PackageGroupCode. Include producer,
profile/ABI and ordered import/export choices. Never normalize nominal versions
because their raw bodies happen to match.

Retained leaves are qualified immutable requirements (symbol, generation, checked
value/interface identity and logical owner correlation), not values or handles.
Keep their actual authorization for installation. Do not hash a whole ambient
World, BindingTipId, current request or runtime address into code identity.

Build reached graph nodes using exact issued occurrence handles. Do not globally
key this work graph by nominal owner/ArtifactId: historical variants could collapse
before validation. Use pinned petgraph 0.8.3 iterative SCC discovery; bound and
charge reached nodes/edges and allocations through existing InventoryOperation.
Canonicalize component members by stable nominal definition keys, preserve ordered
edges/export selectors, and issue dependency components before their parents.
Internal references are component-local; external references name exact child code.
Refuse incompatible duplicate nominal definitions in one component; identical
references to the same issued occurrence are one traversal node. Historical
variants in separate components remain independently selectable.

Unrelated roots, acquisition cohorts and input ordering do not affect identity.
Changed descendants propagate only to dependent components. No whole-inventory
search, per-root graph rebuild, graph-isomorphism rescue or global interner.

## Lazy availability and source meaning

Only demanded code is linked. Dormant declarations keep issued immutable availability
and frozen dependency contracts. They do not require eager CodeRefs for every
export. When demanded, exact source selection authorizes a specific CodeRef; a new
CodeId is not permission to change an existing selected source head. Preserve old
historical dependencies and genuine conflict refusals without 'latest' lookup.

Delete inherited-candidate reconciliation used to rescue unstable executable
identity, completion-row uniqueness keyed only by raw HOME binding, and consumer
reconstruction after their replacements are in production. Acquisition graphs may
remain where they prove acquisition; they no longer define executable identity.

PACKAGE acquisition retains the existing authenticated `PackageLinks` receipt,
not the obsolete executable `ArtifactBinding` scope. Its durable reached
projection retains stage identity, exact occurrence/product references, ordered
child occurrence IDs and occurrence-to-CodeLocation correlation. Restore acquires
the exact certified products and checks the same selected edges through the issuer.
An envelope plus interface seals alone cannot establish the selected child body.
Neither bare ArtifactId lookup nor CodeId equality substitutes for that receipt.

## Acceptance

An actual HOME/PACKAGE issuer-to-installation slice precedes wholesale migration.
Independent properties cover irrelevant roots/order, actual descendant/body/export
changes, cycles, exact retained requirements and distinct nominal definitions.
Keep foreign-grant refusals even when code identities are equal. Restore validates
canonical/maximal SCCs and rejects cycles between stored components. Do not use
the old differing-scoped-bindings test as proof of different semantic children.

## Implementation and remaining qualification

The pure repr component/ref/body/edge owner uses iterative SCC discovery and
same-linker restoration. Its first three deterministic controls executed 3/3 at
joined source `00a05f565a`; later encoder, allocation and independent-property
parcels require their own warm joined qualification.

The actual toolchain demand producer now links exact HOME bindings and selected
PACKAGE occurrences before SCC issuance. An opaque HOME grant retains its
certified ordered SOURCE/retained choices; PACKAGE grants retain the reached
projection of their actual receipt and authenticated products. Acquired imports
pair each pure edge with its issued occurrence selector. Receiving-target Current
correlation remains outside persisted grants. Equal CodeId never supplies it.

New demand may borrow an intact held HOME witness only for the exact binding,
same product issuance and equal closed code. PACKAGE receipt stage/occurrence is
local correlation rather than an extra live entitlement. An explicitly selected
owner is preserved intact; it cannot be rebound through this lookup.

Live and restored acquisitions use one mandatory issuer for exact ordered
product/interface/export and SOURCE/retained checks. Restoration validates each
context's own records and acquired member bodies before sharing canonical
components through the existing operation-local recovery validation owner.
Missing current facts/products cannot be filled from that cache. HOME custody is
narrowed to demanded rows; empty/package-only targets retain no unrelated available
HOME inventory.

Core producer/restore and new actual-target authentication assertions are still
source-only. Compile the joined toolchain production and cfg(test) consumers,
then execute existing real World native and mixed-stage histories. These controls
round-trip their actual target grants, check shared immutable restored owners and
Historical-only restoration, and reject absent current facts/products and forged
scope/selector/leaf-receipt data. Remaining independent acceptance includes actual
different-cohort borrowing, changed dependency and retained/source-owner refusal,
full snapshot writer/reader sharing, runtime installation histories, and final
old-route deletion audit. No intermediate adapter or old graph is accepted as a
permanent second executable path.
