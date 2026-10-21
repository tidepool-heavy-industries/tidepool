# Shared contracts for this wave

These are implementation constraints, not optional review suggestions. P records
the concrete wire layout/version and owner APIs here before P1/P2 edit opposite
sides. W and N record their concrete owner types without changing these semantics.
Conceptual names below are illustrative; reuse good existing names and owners.

## 1. Retain the actual package choice

The producer knows a target's package closure while the values are still paired
in `PreparedPackageTarget`. Preserve that relation through output publication.
Byte deduplication must not deduplicate distinct occurrences or dependency
histories. Fresh HOME issuers need their own exact relation; they are not
automatically covered by a target-only map. Retained HOME edges already issued
must pass through unchanged.

| Identity | What it establishes | What it does not establish |
|---|---|---|
| Envelope SHA | Exact encoded package-group bytes | Imported interface certification or selected children |
| Certified product SHA | Domain-separated commitment to envelope SHA and ordered import interface seals | One exact held dependency history |
| ArtifactId | Existing package descriptor identity | Selection or permission by itself |
| ArtifactBinding / PackageCodeBinding | Exact issued occurrence with its ordered dependency bindings | Authority for a different owner/view |

Keep distinct types at conversions. `package_code.rs` currently defines the
product commitment as SHA256 of `tidepool.package-code-product.v1\0`, envelope
SHA, and ordered import-interface seals. Do not change that hash as a shortcut
for adding selected-child identity.

### Chosen design

Keep immutable payload bytes and a receipt-scoped, finite **occurrence/link
manifest** together under one private producer result. Within that result,
ordered target/HOME import slots refer to specific occurrences; occurrence
imports refer to specific local occurrences or explicit already-issued bindings.
An output-local index is valid only under the authenticated output that issued
it. Runtime binding authority still comes from the existing graph owner.

Use `IssuedPackageCodeGraph` and `ScopeProofV3` local/external edges. Do not put
recursive child digests inside payload bytes, infer a package DAG, or add a
second graph issuer. Preserve cycles through the existing finite scheme where
the producer permits them; otherwise retain the actual existing refusal rather
than silently topologically sorting away a cycle.

`PackageCodePlan` currently keys a planned node by ArtifactId and refuses equal
payload with different ordered children in one plan. Prefer independent issuance
scopes per projection and exact held terminals when two histories share bytes.
If a real single projection needs two such occurrences, minimize it and bring
the graph-owner extension to S/P review before changing that key. Never overwrite
one row or merge child choices because its product hash matches.

### Facts the concrete contract must fill in

- The issuer and lifetime of projection/occurrence coordinates; target slot,
  fresh HOME group and transitive package mappings, including repeated slots.
- Where inherited exact references remain owned across the request. A wire
  reference must be authenticated against its issuing input/output; writing an
  ArtifactBinding-shaped record is not issuing authority.
- The single paired value consumed by writers and Rust decoding; invalid partial
  products/links must not be publicly constructible.
- Decoder checks for missing, duplicate, crossed-scope and out-of-range links,
  conflicting owner/seal/export/signature/representation/evaluatedness, and
  producer/profile mismatch. Keep the existing checks; select more precisely.
- Ordered import counts and the relationship to the actual prepared program,
  not just a count shared by two mutually buggy metadata producers.
- Concrete old/new versions and recovery rules. B authorizes a necessary
  internal receipt/version break, atomically across producer, decoder and
  fixtures. Reject forms lacking required choices; do not reconstruct choices
  for backward compatibility. Preserve exact already-issued graphs if their
  unchanged format remains valid. TPGRP payload bytes need not change merely
  because a separate linkage receipt does.

## 2. A storage path is not a new interface selection

Existing certified interface/custody owners authenticate bytes and selected
scope. An incoming observation may be classified as reuse of an already-issued
selected interface or an actual new selection. Preserve the inherited witness
on reuse, because catalogue additions otherwise override it. Keep original
selection provenance, observed/captured location and authenticated bytes legible.

Same bytes alone do not grant a new compiler/source/owner selection. Neither
path equality nor global path canonicalization supplies that authority. A failed
read stays inaccessible/corrupt/missing as appropriate, not absence or success.
Use the current selected basis and existing issuance owner, not a broad retained
inventory. No global weakening of `PackageInterfaceWitness` equality.

## 3. Publish actual compiler progress

Private execution admission owns its compiler base and lineage. Carry a typed
account of progress through execution, freeze, claim and commit:

- Authenticated unchanged compiler state for a no-cell invocation.
- Actual completed checked-cell successor, from its existing issuer.
- Started/incomplete/refused/uncertain work, which cannot become either success.

An absent prefix proves none of these. Neither a tool name nor Tool-versus-Cell
dispatch alone proves that no compiler facts changed. Installed handlers and
hooks may create retained values; those writes require their actual issuer and
must survive the same publication transaction. If another real progress kind
exists, model its issuer explicitly rather than a wildcard successful branch.

The unchanged case must preserve concurrent valid public progress rather than
restore an older borrowed World. Validate the existing base/owner/source fences
at the owning transaction. Do not introduce Arc-address identity as a substitute
for logical issuance, fake a zero-cell successor, or bypass publication.

Keep cancellation claim order, durable commit, effect receipts, retained semantic
failure output, cleanup ownership and recovery-required state. A completed tool
reply followed by publication failure is not permission to replay its effects.

## 4. Tests and diagnostics tell the truth

Production issuance creates authority in positive tests. Malformed transport may
be mutated to test rejection, but it must never become a test-only authority
factory. Tests of intentionally narrower component APIs cannot qualify World
publication or package execution. Compile changed test consumers early.

Each regression needs a positive history and a distinguishing negative or
metamorphic relation. Explain why it fails under the old mechanism, and why its
oracle is independent of the repaired index/selector. Do not just compare the
new implementation with another call to itself.

Logging is diagnostic. Timing/retention/reuse/END records have their existing
protocol owners and consumers; changing their channel requires migrating those
consumers. Cancellation is not an ordinary logging failure. Error composition
keeps the original failure plus secondary cleanup facts.

## 5. Scope and escalation

Allowed: replace flawed internal APIs, remove legacy entrypoints, version a
required internal receipt, and add small adjacent controls. Not an implicit part
of B: whole-package compilation, a second compiler path, new scheduler, generic
resource/logging framework, recursive content-hash scheme, arbitrary first-match
selection, relaxed runtime authority, or deferred ordinary G2 correctness.

For a genuine design obstacle, preserve the smallest real producer/consumer
counterexample, show at least two alternatives and recommend one. Continue the
independent frontier. Do not repeatedly delegate the same unresolved question or
paper it over with an Option, string marker, broad catch, or compatibility shim.
