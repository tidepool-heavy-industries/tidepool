# P subtree: exact package occurrence linkage

Primary outcome: a consumer uses the exact package occurrence its compiler
projection chose, including transitive and historical children. Secondary
questions: avoid duplicated encoding/graph snapshots; delete symbol-rematching
routes; make malformed linkage fail at one typed decoder boundary.

Read CONTRACTS and `peer-review-package-occurrences.md` before proposing types.
The review already rejects a global symbol-to-digest map and recursive child
hashes. These are not open implementation alternatives.

## Trace to preserve

At the reviewed base:

- `ExecutionProjection.hs::PreparedPackageTarget` pairs code with package groups;
  per-target export uniqueness is established around lines 953–977.
- `CompilerRequest.hs` carries per-entry results around 791/837–879 but removes
  entry keys and flattens captures around 936–947.
- `CompilerProducts.hs::retainPreparedPackageTargets` flattens groups into
  byte writes and interface captures around 495–534. This is the information
  loss, before Rust's ambiguous `(seal, symbol)` search.
- `CertifiedProducts.hs` issues fresh HOME group certificates separately
  (around 415–432 and `packageOwner` around 832). Existing inherited HOME rows
  already have issued edges. Both consumers must receive the right kind of link.
- `artifact_inventory/compiler_projection.rs::complete_native_selection`
  performs the downstream ambiguous selection.
- `package_code.rs` owns envelope authentication and certified product identity.
- `artifact_inventory.rs::IssuedPackageCodeGraph` owns exact selected package
  rows; `scope_proof.rs` owns finite local/external proof references. Runtime
  prepared code already indexes exact ArtifactBindings.

Verify symbols at the new tip; line numbers are review locators, not APIs.

## P: contract and subtree integration

Publish a concrete contract supplement before P1/P2 code changes. Include a
small diagram/table for two targets sharing a package symbol with different
products, transitive A→B, and the same payload under two child histories. Name
which object retains each choice, how it is encoded, and where Rust authenticates
it. Include fresh HOME and carried HOME, not only expression targets.

Choose an output-local occurrence coordinate confined to the paired completed
receipt; do not require Haskell to impersonate Rust binding authority. Share
payload bytes separately from occurrence rows. Use exact inherited binding
references through their existing authenticated input owner.

State versions and cold-recovery behavior explicitly. Changing the manifest
does not automatically require changing TPGRP-v2 byte encoding. The Rust product
digest is not the filename's envelope SHA. If a digest computation crosses
languages, freeze one independent vector and test the real encoder/decoder.
Prefer not to duplicate the product-hash algorithm when a scoped envelope
coordinate under authenticated output is sufficient.

Review whether per-projection issuance avoids `PackageCodePlan`'s one-ArtifactId
key restriction. If it does, retain it. If not, require the smallest actual
producer case before changing graph representation. There must still be one
graph issuer and one finite proof scheme.

P owns conflict resolution and joins P1/P2/P3 onto one branch. Compile a tiny
real producer→decoder→installation history early, before broad notebook runs.
Do not send S an uncompiled wire change with two independently passing halves.

## P1: Haskell producer

Own the paired target/occurrence construction and its passage through
CompilerRequest, CompilerProducts, ExecutionProjection and the necessary
CertifiedProducts writer hunks. Agree exact shared hunks with W/Q3/C.

Construct the opaque paired result where the actual projection choices are
available. The writer consumes that result, not a broader inventory to make a
second selection. A cache/dedup map may share encoded bytes; it must not erase
projection membership, slot multiplicity, selected children or original order.

For package-to-package imports, retain the chosen occurrence while the projection
still knows it. For freshly certified HOME groups, use their issuing projection
or an equally exact existing compiler relation; don't attach whatever target
map happened to be nearby. Keep inherited exact relationships as inherited.

Return typed/refined failure at the owning construction boundary if a demanded
link cannot be issued. Do not emit an incomplete optional manifest and rely on
Rust fallback. Avoid encoding every group twice just to recover its identity.
Immutable encoded bytes may carry their digest and proof facts together.

## P2: Rust admission and recovery

Own decoding/authentication, conversion to existing package plans/graphs and
native slot selection. Keep envelope/product/ArtifactId/binding types distinct
at the conversion points. Internal raw representations may be decoded, but
downstream consumers receive one authenticated paired value.

Resolve ordered slots directly by their exact local/external references under
the issuing owner. Retain all existing owner/seal/export/contract checks.
Delete the ambiguous symbol/seal selection API and callers. A digest-to-many
inventory can retain alternatives; it cannot choose one exact held history.

Restore persisted issued graphs through their existing owner. Reject obsolete
or incomplete linkage rather than synthesize it from an inventory or latest
module map. Preserve sharing of immutable bytes/proofs; do not clone an entire
dependency graph into each import. Make failures identify the projection/slot
and missing/conflicting relation without dumping every retained artifact.

## P3: independent oracle and category controls

Own agreed new fixtures/controls, not production issuer internals. Read the
actual producer schema and consuming prepared-program imports independently.
Prove that a regression to `(seal, symbol)` or first-match would fail the tests.
Use actual compiler issuance for valid values; mutate transport only for negative
decoding controls. No test-only certificate constructors or post-hoc SQL/state
repair. Fixed Haskell programs belong in declared adjacent fixture files.

Required relationships, combined into a few meaningful histories where possible:

- Two projections with the same symbol/seal and different products. Reverse
  target order; each still uses its own selected implementation.
- A package A import of B while another target supplies a competing B. Verify
  the transitive choice, not just root selection or a matching digest.
- Repeated slots and shared identical leaves retain slot multiplicity while
  sharing immutable bytes. Distinct child histories remain distinct bindings.
- Fresh HOME imports and retained historical HOME imports select correctly;
  custody-only alternatives cannot become executable demand.
- Wrong projection, unissued/missing/out-of-range reference, changed owner,
  export, representation, signature, evaluatedness or producer/profile refuse
  before installation/effects. Keep precise typed causes.
- Recover two issued histories and select each separately after disposable
  producer outputs are removed. Do not reconstruct from a latest-head map.
- Exercise the graph's finite cyclic linkage where supported, or test its
  existing explicit producer refusal; don't invent a new DAG assumption.

Use a small reference event model or generated producer histories when it adds
coverage of order/aliasing/reuse. Runtime evaluation or independent GHC behavior
should establish semantic results; the new manifest must not verify itself.

P3 reviews the joined production diff. R independently reviews P3's oracles and
the exact joined subtree. Run affected fixture/serialization checks, then send
S one reviewed candidate and explicit pending joined gates.
