# P3: independent package occurrence oracle controls

Owner role P3. Implemented against the frozen contract supplement
`CONTRACTS.md §1a` (tip `6c311e7d5f`). This file records the discriminating
fixtures and controls, and why each one fails under a regression to the old
`(seal, symbol)` / first-match selector.

## Producer/consumer trace (read independently)

- `ExecutionProjection.hs::projectPreparedPackageTarget` pairs a target with
  its exact `[ProjectedPackageGroup]`; per-target export uniqueness is checked
  at the same call.
- `CompilerRequest.hs` stores each target's `NativeFinal` and, at the
  flattening step, drops the entry key and concatenates every target's groups.
- `CompilerProducts.hs::retainPreparedPackageTargets` flattens the groups into
  envelope-SHA-deduplicated byte writes plus interface captures; target
  membership and ordered import slots are gone at this point.
- `CertifiedProducts.hs::packageOwner` records only `(unit, module, seal)`, so
  it cannot name one of two bodies sharing a module and seal.
- `artifact_inventory/compiler_projection.rs::complete_native_selection`
  performs the downstream `(seal, symbol)` search; the fallback in
  `package_target` refuses "competing captured code products" when two bodies
  match, or first-matches an inherited `IssuedPackageCodeGraph`.

## Fixtures (`tidepool/toolchain/tests/fixtures/package-occurrence-linkage/`)

Each fixture is a self-contained module whose targets import real GHC package
modules. No fixture supplies package authority; the compiler issues it.

| Fixture | Scenario it discriminates | Regression it catches |
|---|---|---|
| `SameSealTwoProducts.hs` | `onlyPack` reaches `Data.Text.Internal.pack` alone; `packThenUnpack` reaches `pack` + `unpack`. The current compiler emits package products per binding, so both targets resolve the shared `pack` worker to one immutable envelope. | Reversing target order must not change either target's exact ordered slots, body, or child history. |
| `TransitiveCompetitor.hs` | `viaText` reaches `Data.Text.Internal` transitively through `Data.Text`; `directText` reaches it directly. | Old selector cannot keep the transitive child distinct from the direct sibling projection. |
| `RepeatedSlots.hs` | Three `sort` demands share one immutable `actualSort` body plus one Ord dictionary. | Byte dedup must not duplicate the retained payload. |
| `DuplicateEnvelope.hs` | One defining module reached under two different child histories. | Envelope-SHA keying collapses two occurrences; OccurrenceId keeps them distinct. |

## Controls (`artifact_inventory/package_occurrence_linkage_tests.rs`)

Positive histories run `test_support::compile_invocation` (real issuance); they
observe `CompletedNativeSelection::target_imports()` and the retained
`PackageCodeBinding::product()` bytes. Each is `#[ignore]`d because it needs the
rooted compiler service, and registered in the counted runner target
`toolchain_package_occurrence_linkage_tests` (`ignored = True`, owned-resident).

The assertions are written against the physical selection, so the old selector
fails at the compile step (competing products) or by selecting the wrong body;
the oracle does not compare the new implementation with itself.

## Negative decoding controls (pending P2 decoder)

The following transport-mutation matrix is queued behind the P2 decoder and
must land with it; each case mutates one field of a real decoded receipt and
asserts a precise typed refusal, never a test-only authority constructor:

1. `package_links` missing or old version -> `Receipt("complete product output variant")` / unsupported version.
2. Local `OccurrenceId` out of `[0, occurrences.len())`, duplicate row id, or byte-identical rows -> occurrence/row refusal.
3. Row `envelope_sha256` absent from `package_products` -> crossed-scope refusal.
4. `imports.len() != product.code().globals().len()` or slot-count mismatch -> `ImportCount`.
5. Changed owner / defining interface / defining version -> crossed-scope owner refusal.
6. Changed producer/profile -> `check_product` refusal.
7. Changed export / representation / signature / evaluatedness -> `ImportContract` / `ExportUnavailable`.
8. `Link::Inherited` with zero or two exact held bindings -> refusal, never seal+symbol first-match.

## Recovery controls (pending P2)

Two issued histories must be recoverable through `from_issued` after disposable
producer outputs are removed, and each selected separately without a
latest-head reconstruction. This is queued with the negative matrix.

## Trace findings that steer the discriminator (2026-10-21)

Running the controls through the real compiler (owned-resident) established
three facts that correct the initial fixture premise:

1. **Package products are per-binding, not per-module.** `GHC.Internal.Data.OldList`
   emits separate envelopes for `sort1`, `sort2`, `sort3`, `actualSort`,
   `unlines`, and the Ord dictionaries. Two targets that reach different
   subsets of one module therefore keep byte-identical bodies for a shared
   symbol: `actualSort` resolves to the same envelope whether or not the other
   target also reaches `unlines`.
2. **The observed "competing products" arise from constructor/type metadata
   renumbering, not reachability.** `peer-review-package-byte-comparison.json`
   shows equal binders with differing constructor/frame/type fields for
   `GHC.CString.unpackCString#` and `Data.Text.Internal.$wouter`. A
   discriminating positive fixture must reproduce that renumbering; a simple
   same-module/different-subset fixture does not. This is now a first-class
   P1/P3 coordination item before the positive same-seal control can be
   claimed.
3. **The compiler deduplicates identical package-global references.** Three
   `sort` calls in one target become one `actualSort` global plus one `gtInt`
   dictionary, not three slots. Slot multiplicity at the manifest level is the
   producer's ordered `target_roots`/`home_roots`/`imports` lists, not the
   deduplicated physical global list.

The current controls therefore assert the properties that are already true and
must remain true (exact ordered slot selection survives reversed target order;
shared bytes are not duplicated), while the same-seal/different-body positive,
the transitive competitor, and the negative/recovery matrices remain queued
behind the renumbering fixture and the P2 decoder.

## Joined-tip review findings (P1 `dfeff6e569`, P2 `3c661591a1`)

The joined producer/consumer tips expose three concrete defects that the
oracle controls are written to catch. All three are first-class P3 findings,
not hypothetical cases.

1. **Cross-language receipt version mismatch.** The producer now emits
   `TPCERT` version 12 (`CertifiedProducts.hs`, `encodeWord 12`), but P2's
   committed decoder still accepts only version 11
   (`certified_products.rs::product_receipt_payload`). Every native fixture
   compile refuses `UnsupportedVersion { found: 12, expected: 11 }`. The
   version bump must be committed atomically with the decoder; P2's
   uncommitted working tree contains the matching bump.
2. **`home_roots` keyed owner-only in the producer.** P1's
   `packageLinksHomeRoots` is `Map (unit, module) [PackageLink]` and is built
   with `Map.fromListWith (flip (++))`, merging the package roots of every
   original ordinal of one HOME module. P2's working decoder expects
   `(unit, module, original_ordinal)`. `MultiOrdinalHome.hs` supplies a module
   with two ordinals whose package roots differ, so owner-only keys or cursor
   reuse assigns one ordinal's roots to the other.
3. **Request-wide binder-to-occurrence map is last-write-wins.** P1 builds one
   `Map SymbolIdentity Word32` (`packageLinkIndexExportOwner`) across every
   target's encodings. When the same exported symbol appears in two target
   occurrences (for example `actualSort` reached at `Int` versus `String`), the
   map keeps one entry, so both targets' slots resolve to one occurrence. The
   target link must come from that target's own `PreparedPackageTarget`, and a
   parent occurrence's imports from that parent's projection.

`PackageLink::Inherited` is refused everywhere in P2's committed decoder
("fresh package occurrence cannot import an inherited terminal"); the exact
held-terminal resolution (`resolve_held_package_binding`: one exact binding,
zero/multiple refusal) is in P2's uncommitted tree, so the Inherited
zero/one/many oracle is queued behind it.

## Execution at joined tips P1 `f4d268bafb` + P2 `83ac54b3aa` + contract `f2026b45b4`

The occurrence cohort (`toolchain_package_occurrence_linkage_tests`, 5 cases,
owned-resident) runs against the version-12 joined tree:

- `transitive_import_prefers_exact_child_over_competing_sibling` PASS.
- `repeated_slots_share_immutable_bytes_without_losing_multiplicity` PASS.
- `multi_ordinal_home_keeps_per_ordinal_package_roots` PASS: the
  ordinal-qualified `(unit,module,original_ordinal)` HOME roots resolve, and
  P1's claim that fresh HOME roots may use the first target's occurrence map
  holds for the two-ordinal HOME case.
- `reversed_targets_keep_exact_ordered_package_slots` and
  `duplicate_envelope_histories_remain_distinct_bindings` FAIL with the first
  exact product defect below.

### First exact product defect

`CompilerEvidence(Receipt("duplicate package occurrence row"))`. P1's
per-target occurrence emission keeps one row per `(target, group)`, so a
symbol that two targets share with the same encoded body and the same ordered
child history (`actualSort` under `Int`/`String`, or `Data.Text.pack` under
two projections) is emitted twice. The contract requires one row per distinct
(envelope, ordered child history); byte-identical rows are a producer-defect
refusal. P1 must deduplicate byte-identical occurrences and share the single
row across both targets' `target_roots`, after which the reversed-targets
oracle passes with the shared slot resolving to one body.

The producer's per-target binder map is now correctly target-scoped, so the
remaining failure is not the request-wide-map collapse; it is the missing
byte-identical-row deduplication. P1's bisimulation dedup (merged through the
P tip `420fdf7ebb`) fixes that refusal, so the reversed-targets oracle keeps
its truthful order/reverse-order assertion and no longer expects distinct body
bytes for one shared symbol.

## Authenticated historical same-seal/different-body control

The synthetic same-module/different-subset fixture cannot reproduce the
constructor/type renumbering, so the same-seal/different-body positive is
covered by four authenticated compiler envelopes under
`tests/fixtures/package-occurrence-linkage/historical/` (plus two captured
interfaces and `manifest.json`). The non-ignored test
`historical_same_seal_bodies_select_separately_and_refuse_seal_symbol_match`
decodes each envelope through the production reader
(`CertifiedPackageCodeProduct::restore` -> `parse_code` ->
`PackageGroupCode::admit`), admits both bodies under one shared owner/seal,
selects each by its own occurrence `ArtifactId` through
`IssuedPackageCodeGraph::issue`, and proves `resolve_held_package_binding`
refuses the two-body seal+symbol match while accepting each body alone.
