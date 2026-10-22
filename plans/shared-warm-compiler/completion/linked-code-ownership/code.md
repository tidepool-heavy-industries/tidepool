# One demanded linked-code graph

B owns the toolchain graph/issuer and required pure identity types. Coordinate
runtime and snapshot APIs directly with A/D, with root copied on contract changes.

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

## Acceptance

An actual HOME/PACKAGE issuer-to-installation slice precedes wholesale migration.
Independent properties cover irrelevant roots/order, actual descendant/body/export
changes, cycles, exact retained requirements and distinct nominal definitions.
Keep foreign-grant refusals even when code identities are equal. Restore validates
canonical/maximal SCCs and rejects cycles between stored components. Do not use
the old differing-scoped-bindings test as proof of different semantic children.
