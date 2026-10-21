# W: preserve interface selection across retained copies

Primary outcome: reusing the same selected package interface through a retained
copy does not masquerade as replacing the selection. Secondary questions:
preserve origin versus location, remove duplicate authentication policy, and
ensure missing/inaccessible/corrupt storage remains distinguishable.

## Confirmed counterexample

Two real notebook cases stop in
`certified_products.rs::check_segment_package_addition` around 923–944.
`PackageInterfaceWitness` equality includes `selected_path` and SHA. Incoming
GHC.Tuple names a Nix interface; inherited evidence names a transaction copy.
Both files were independently read and have seal
`422dc5ab45c301194958fcf77f3cd85da1cf06ca83509c92f84036298a292af8`.

Read `peer-review-package-witnesses.md` and its JSON evidence for exact paths.
This is independent of package-code occurrence selection. Keep both failures
visible until each is repaired.

## Chosen repair boundary

Use the existing certified interface/custody owner to classify an authenticated
incoming observation as reuse of an already-issued selected interface or a new
selection. Preserve the inherited witness on reuse. Catalogue additions have
precedence, so merely relaxing the guard would replace the carried witness.

Existing owners to reuse:

- `CertifiedPackageInterface` for captured unit/module/seal/bytes.
- `PackageInterfaceCustody::bind_witness` in package_code.rs.
- `PackageInterfaceValidation::register_package_interface` in recovery.rs.
- `check_direct_package_agreement_with_custody` in certified_products.rs.

The result should carry the selected authority and its byte evidence together.
Do not have callers redo equality, reopen paths and decide whether to overwrite
the catalogue. Keep constructor access narrow and consume this result at the
existing admission boundary. Same bytes alone do not authorize a new selection.

Trace the Haskell source too: `captureSelectedPackageWitnesses` copies bytes and
rewrites receipt paths. Later item capture can fall back to ambient GHC when
the current stage lacks an inherited witness. Record those facts accurately;
do not infer that a new path means a new compiler choice.

A producer carry-forward repair is an alternative only if the selected-owner
classification cannot retain the actual provenance. It must carry bytes when
old transaction paths disappear, and coordinate P's writer changes. Do not
create a global path registry or rehydrate every historical interface eagerly.

## Acceptance and review

Use original capture → retained copy → later item → repeat → producer removal
and cold reopen through real admission. Include positive relocation and wrong
owner/seal/source/compiler selection, absent, inaccessible and corrupt bytes.
An incoming duplicate row must not silently override inherited evidence.

The old helper test that rejects every different path encodes the wrong premise.
Replace that setup with a production-issued history; keep genuine different
selection refusals. Don't just reverse its assertion or globally change Eq.

Run the two unchanged notebook histories once joined with the defining-Name
prerequisite. Record other blockers separately. Compile production plus affected
test consumers and request R review before integration. In the report show the
old and new dataflow, where selection is issued, where the copied location lives,
and which obsolete validation route was removed.
