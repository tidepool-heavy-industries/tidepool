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

## 1a. P supplement — frozen occurrence/linkage contract (2026-10-21)

Authored by role P after tracing the producer/consumer at worker base
`0cfb964739`. This is the concrete fill-in of §1's "Facts the concrete contract
must fill in". It freezes the wire, lifetime, version and identity rules and
the producer -> decoder -> install controls. P1/P2/P3 implement against it and
do not relax it; any change returns to S/P review.

### Trace verdict: where the relation is lost

Haskell pairs a target with its exact package products in
`PreparedPackageTarget` (`ExecutionProjection.hs:947`), built in
`projectPreparedPackageTarget` (`ExecutionProjection.hs:953`) where exported
binder uniqueness is enforced per projection (`ExecutionProjection.hs:970`).
`CompilerRequest.hs` stores the paired result as `NativeFinal` per projection
entry and, at `CompilerRequest.hs:936-947`, drops the entry key and flattens:

```haskell
finalResults <- Map.elems <$> readIORef finalProjections
let packageTargets = [(target,captures) | NativeFinal target captures _ <- finalResults]
...
Just <$> retainPreparedPackageTargets provenance (map fst packageTargets) captures products
```

`retainPreparedPackageTargets` (`CompilerProducts.hs:504`) performs the loss:
`groups = concatMap packageTargetGroups targets` (`CompilerProducts.hs:514`)
flattens every target's `ProjectedPackageGroup` into one list, encodes each as
an envelope (`CompilerProducts.hs:523-527`) and deduplicates writes by envelope
SHA (`CompilerProducts.hs:532`). The returned `PreparedPackageOutputs`
(`CompilerProducts.hs:495`) keeps only envelope files and a
`(unit,module) -> interface capture` map. Both target-to-occurrence membership
and occurrence-to-occurrence import order are gone at this point.

Fresh HOME certification resolves package imports independently:
`CertifiedProducts.hs:290` `resolvePackage` looks up `(unit,module)` in
`capturedPackages` and returns a `PackageImportRoot`, which `packageOwner`
(`CertifiedProducts.hs:832`) records as `PackageOwner unit module sha`. That is
module plus interface seal only, so it cannot name one of two bodies sharing a
module and seal.

Rust admission consumes the ambiguous remnant in `compiler_projection.rs:1046`
`package_target(global, seal)`: it filters fresh products by
`defining_interface().sha256() == seal` plus exported symbol, then falls back to
a seal+symbol search over inherited `IssuedPackageCodeGraph`s
(`compiler_projection.rs:1069-1078`), refusing "competing captured code
products" when two bodies match. This is the exact downstream ambiguity this
contract removes.

### Occurrence coordinate and lifetime

- The **envelope SHA** (`sha256(encodePackageGroup ...)`, the retained filename
  and `PackageProductOutput.sha256`) is the immutable payload identity used for
  byte deduplication. It is unique per distinct encoded package body but is
  **not** an occurrence, selection or binding identity: the same envelope can
  be certified under different ordered import seals and therefore yield
  different `ArtifactId`s.
- An **`OccurrenceId`** is a producer-local `u32` assigned by the writer,
  receipt-scoped and stable within one manifest (equal to the row's position in
  `occurrences`). It is distinct from envelope SHA, product SHA, `ArtifactId`
  and `ArtifactBinding`. Every link and every target/HOME root resolves through
  an `OccurrenceId`, never through an envelope or payload-map lookup.
- An **occurrence row** is
  `{ id, envelope_sha256, unit, module, defining_interface_sha256,
  defining_version_sha256, imports }`. Rows are ordered by `id`. Two rows may
  share one `envelope_sha256` only when their ordered import histories differ;
  their identity remains their distinct `id`.
- `Link = Local { occurrence: OccurrenceId }` (an exact fresh occurrence in
  this same output) or `Inherited` (an exact already-issued occurrence; see
  below).
- A local `OccurrenceId` reference is valid only under the authenticated output
  that issued the manifest. The manifest never becomes durable authority;
  runtime binding authority stays with `IssuedPackageCodeGraph` /
  `ScopeProofV3`. `Link::Inherited` is a demand marker, never an
  `ArtifactBinding`-shaped record written by Haskell: the exact binding is
  resolved on the Rust side from the exact inherited graphs already carried in
  that completion, or refused.

### Two kinds of collision, one coordinate

| History | What differs | What the contract does |
|---|---|---|
| Two targets, same symbol/seal, different bodies | Same defining interface seal; different envelope SHA | Distinct `OccurrenceRow`s; each target's `target_roots` names its row by `OccurrenceId` |
| Transitive A -> B with a competing B | A's ordered child slot points at one B, another target supplies the other B | A's `imports` names B's `OccurrenceId`; the other B stays custody but is never selected |
| Same payload under two child histories | Same envelope SHA; two distinct rows with different ordered `imports` | Two `OccurrenceRow`s share `envelope_sha256` but have distinct `id`s; links stay unambiguous. If their ordered import seals differ they yield distinct `ArtifactId`s and issue as two plans; if they collapse to one `ArtifactId`, `issue` refuses `ConflictingPlan` (typed), never a silent merge |

### Wire and version

- Add a required field `package_links` (the occurrence/linkage manifest) to the
  completed-product receipt and bump the receipt layout once, atomically:
  - Haskell encode `CertifiedProducts.hs:520-541` (`TPCERT`, current version 11)
    gains one payload element for both the `"ordinary"` and
    `"segment-originals"` variants.
  - Rust decode `decode_receipt_value_in` (`certified_products.rs`, array
    lengths 9/8 today) accepts only the new length and refuses the old length
    as `Receipt("complete product output variant")`.
- Reject any receipt lacking the manifest; do not reconstruct choices for
  backward compatibility. `TPPACKAGEGROUP1`/TPGRP envelope bytes, the
  `tidepool.package-code-product.v1` product hash (`package_code.rs:559,655,777`)
  and `ArtifactDescriptor::package_code` (`artifact_inventory.rs:1974`) are
  unchanged.
- Already-issued graphs whose persisted format is unchanged remain valid and
  are reborrowed through `from_issued` (`artifact_inventory.rs:23310`).

### Manifest shape (one paired value)

```text
PackageLinks {
  version: u8 = 1,
  occurrences: [OccurrenceRow],              # ordered by id == position
  target_roots: Map<String, [Link]>,         # key = CertifiedReceipt.targets key;
                                             # one Link per non-HOME package
                                             # global in that target's program
                                             # order
  home_roots: Map<(String,String), [Link]>,  # key = fresh HOME group (unit,module);
                                             # one Link per package global in
                                             # that group's program order
}
OccurrenceRow {
  id: u32,                                   # == position in `occurrences`
  envelope_sha256: [u8;32],                  # shared payload; not the row key
  unit: String, module: String,
  defining_interface_sha256: [u8;32],
  defining_version_sha256: [u8;32],
  imports: [Link],                           # length == code.globals().len()
}
```

`occurrences` holds one row per producer-issued occurrence in this output,
including repeated rows when one envelope is selected under different ordered
children; it is emitted beside the flattened `package_products` list (which
keeps its existing envelope-SHA dedup). The `imports`, `target_roots` and
`home_roots` lists are the single paired value that writers encode and Rust
decodes; there is no independent zip or lookup. A `Link::Local` carries an
`OccurrenceId` that must index a row in `occurrences`; `Link::Inherited` carries
no digest and must never be resolved by search.

### Producer rules

- Build links inside `retainPreparedPackageTargets` from each
  `PreparedPackageTarget.packageTargetGroups` **before** the `concatMap` at
  `CompilerProducts.hs:514`, where the ordered groups are still paired.
- Assign each fresh `ProjectedPackageGroup` a stable `OccurrenceId` in
  deterministic emission order (target order, then group order within each
  target), distinct from the byte-dedup key. The same encoded body may get one
  row per distinct ordered-child history; never key rows by envelope SHA.
- Target slot order is the package globals of `packageTargetProgram` in program
  order (the non-HOME globals computed at `CompilerRequest.hs:849-855`), not an
  arbitrary map order.
- Occurrence import slot order is `projectedGlobals (projectedBody group)` for
  that group's `ProjectedGroupEvidence`, matching
  `CertifiedPackageCodeProduct.code().globals()` order.
- Fresh HOME roots: each fresh HOME group's package imports resolve through the
  same per-target occurrence map that issued them; they are not derived from a
  target-only map or a global `(unit,module)` interface lookup. P1 carries the
  paired value into `encodeCertifiedOriginalProductsWithPrior` beside
  `packageProducts`/`capturedPackages` (`CertifiedProducts.hs:286-297`).
- A cache/dedup map may share encoded bytes; it must not erase projection
  membership, slot multiplicity, selected children, or original order. Refuse
  at the owning construction boundary (typed failure) if a demanded link cannot
  be issued; never emit an incomplete optional manifest with a Rust fallback.

### Decoder checks (typed refusals at one boundary)

1. Manifest present at the new version; every `Link::Local.occurrence` is in
   `[0, occurrences.len())` and row ids are exactly `0..n-1` with no duplicates
   (out-of-range/missing/duplicate id = refusal).
2. Every row's `envelope_sha256` is present in the authenticated
   `package_products` (crossed-scope/foreign occurrence = refusal). Duplicate
   `envelope_sha256` across rows is allowed only with distinct ordered
   `imports`; byte-identical rows are a producer-defect refusal.
3. `occurrences[i].imports.len() == product.code().globals().len()`;
   `target_roots[k].len()` equals that target's package requirement count;
   `home_roots[k].len()` equals that HOME group's package global count.
4. Row `id` is its position; owner, defining interface and defining version must
   equal the authenticated product owner, `defining_interface()` and defining
   version; a Local ref whose row envelope matches but whose owner or defining
   seal disagrees is a crossed-scope refusal.
5. `Link::Inherited` resolves only to the exact held binding already carried in
   the inherited completions for that slot; retained HOME rows pass their prior
   imports through unchanged. Absent or more than one exact held binding is a
   refusal, never seal+symbol first-match.
6. Keep every existing check at conversion/install: producer/profile match
   (`compiler_projection.rs:1031` `check_product`), `validate_export`
   (`artifact_inventory.rs:22881`), `validate_import` (`artifact_inventory.rs:22925`),
   `PackageGroupCode::admit` (`execution_schema.rs:1363`),
   `ProjectedGroup::validate_code_owner` (`execution_schema.rs:1229`), envelope
   authentication (`package_code.rs:707-738`), and the `issue` ImportCount /
   ConflictingPlan / AmbiguousInheritance refusals (`artifact_inventory.rs:22961+`).

### Install and recovery

- The decoder turns each `OccurrenceRow` into
  `PackageCodePlan { product, imports: [PackageCodeTarget] }`: it matches the
  row's `envelope_sha256` to the fresh `CertifiedPackageCodeProduct` with that
  envelope, `Link::Local(i)` becomes
  `PackageCodeTarget::Planned(plan[i].artifact_id())`, and `Link::Inherited`
  becomes `PackageCodeTarget::Held(exact_binding)`.
  `IssuedPackageCodeGraph::issue` (`artifact_inventory.rs:22961`) still owns
  selection, reuse and `ScopeProofV3` issuance. Two rows sharing an envelope
  yield distinct plans when their ordered import seals differ (distinct
  `ArtifactId`); if they produce one `ArtifactId` with different imports,
  `issue` refuses `ConflictingPlan`. The seal+symbol `package_target` search
  (`compiler_projection.rs:1046-1080`) and its competing-product failure are
  deleted with their callers.
- Recovery reborrows exact rows through `from_issued`
  (`artifact_inventory.rs:23310`); it never reconstructs a plan from a
  latest-head inventory or a `(seal, symbol)` lookup.

### Controls (producer -> decoder -> install)

- Two targets with the same symbol/seal but different bodies, reversed target
  order; each target uses its own body after installation (executed or typed
  selection evidence, not just a matching digest).
- Package A imports B while another target supplies a competing B; assert the
  transitive choice.
- Repeated slots and byte-identical shared leaves keep slot multiplicity while
  sharing bytes; distinct child histories remain distinct bindings.
- Fresh HOME imports and retained historical HOME imports select correctly.
- Wrong projection, unissued/missing/out-of-range link, changed owner/export/
  representation/signature/evaluatedness, and producer/profile mismatch refuse
  before installation with precise typed causes.
- Recover two issued histories and select each separately after disposable
  producer outputs are removed.
- A regression to `(seal, symbol)` or first-match must fail these tests; the
  negative oracle is independent of the repaired selector.

### Explicit residual gates for S/P review

- `PackageCodePlan` remains keyed by `ArtifactId` (`artifact_inventory.rs:22814`,
  issue at `artifact_inventory.rs:22961`). The producer-local `OccurrenceId`
  keeps slot and child-history selection exact but does not widen the graph
  key: equal payload with different ordered children resolves to one
  `ArtifactId` only when their ordered import seals also agree, and that case
  is still `ConflictingPlan`. Per-projection issuance scopes are expected to
  avoid it; if one real projection needs two such occurrences under one
  envelope with equal import seals, P stops and brings the graph-owner
  extension to S/P review before widening the key.
- The exact `Link::Inherited` representation (whether a fresh occurrence may
  import an inherited terminal at all, or inherited subtrees always pass
  through a retained parent row) is confirmed by P1's trace against the
  recovery/reuse path before P2 freezes the decoder; either way the decoder
  must not re-introduce seal+symbol search.

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
