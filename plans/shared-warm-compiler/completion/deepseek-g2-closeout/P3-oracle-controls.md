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
| `SameSealTwoProducts.hs` | `onlySort` reaches `sort` alone; `sortThenUnlines` reaches `sort` + `unlines`. Both share the `actualSort` worker symbol under one seal. | Each target keeps its own ordered slots under reversed target order. |
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
