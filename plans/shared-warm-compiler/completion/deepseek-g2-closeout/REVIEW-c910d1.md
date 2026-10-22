# Joined review of c910d1aae0

Date: 2026-10-21. Implementation reviewed:
c910d1aae0e64c76449bef8170b53400133cbd23; cumulative base 1d239474e5.
The tree was clean. Root traced the producer/consumer and publication joins;
four independent Sol reviewers covered packages, publication, witness/roles,
and tests/quality. No new build or product test was run in this review.

**Disposition: changes requested; preserve and repair the integrated candidate.**
Do not roll back the whole wave or claim only one residual blocker remains.

## Evidence and confidence

Detailed reports are retained under
/srv/swarm/evidence/inanna/g2-peer-review-c910d1-20261021/:

- package-review.md: selection, occurrence identity, unnecessary work.
- publication-review.md: original/private/latest selection and host progress.
- witness-roles-review.md: exact historical edges versus namespace/custody.
- tests-quality-review.md: claims versus assertions and bounded quality findings.
- segment-item-artifact-analysis.json: independent retained-CBOR inspection.
- fresh-home-cursor-branch-model.json and owner-closure-counterexample-model.json:
  finite control-flow models, **not production execution**.

The package reviewer additionally examined deletion of graph canonicalization
and whether newly issued projections need an inherited marker. Reports supply
source locators at the reviewed OID. The next workers must obtain executed
evidence for their repairs.

## Findings to act on

| ID | Finding and origin | Required repair owner |
|---|---|---|
| P-ITEM | Confirmed retained failure: item has package globals, but no paired roots reach Rust | P producer/consumer |
| P-HOME | Newly added HOME resolver unions target environments; unrelated targets affect selection | P1 |
| P-REUSE | Fresh HOME with prior completion obtains roots but does not advance the validation cursor | P2 |
| P-SCOPE | All occurrence rows become ArtifactId-keyed plans before demand; disjoint histories interfere | P2 |
| P-HELD | Existing inherited seal/symbol census survives the intended exact-selection migration | P1/P2 |
| H-SELECT | New owner-keyed Original census follows/substitutes an unused historical body | H |
| N-MERGE | N's new latest-public equality input permits installation of the old private runtime selection | N1 |
| N-HOST | Latent retained-host-output path changes bindings without a coherent World association | N2 |
| N-START | Started checked work can be classified Unchanged at freeze; owner contract lacks the transition | N2 |
| W-ORDINARY | Pre-existing ordinary catalog still rejects authenticated equal-interface copies by full path equality | W |
| T-ORACLE | Several new passing cases do not assert their advertised adversarial premise | P3/R-P |
| Q-JSON | New handwritten level scanner accepts malformed JSON after recognizing a level | Q |

Except P-ITEM's retained failure, runtime consequences here are source-derived,
not newly executed reproductions. N-START is an owner enforcement gap; normal
actor failure orchestration often avoids publication, so no actor race is claimed.

## P-ITEM: the exact loss

The retained h-run5 item receipt is TPCERT v12, segment-item arity four, with
three direct package globals. Its stage-1 receipt retains nine occurrences
but zero target roots. The earlier stage has zero occurrences/target roots.
The artifact analysis records paths and SHA-256 values.

The producer still knows each item and its package projection after
prepareArtifactsWithProjectionIntent. prepareSegment then rewrites paTarget
to the scaffold name and passes only paProgram into the item writer.
writeCertifiedSegmentProducts calls the shared writer with an empty targets
list. certifyItemProducts emits witnesses/additions but no root selection.
Rust prepare_world_cell_program supplies stage links with target_name=None;
complete_native_selection necessarily refuses on a package slot.

Carry the actual item/program/import pairing before these losses. A later
native ordinal, scaffold name or HOME map is not automatically the same
selection. Prove the relation at issuance; do not guess it in Rust.

## Historical selection is not an owner census

world_stage_fragment builds original_by_owner from every retained Original
with availability. Last map insertion wins. It follows that body's requirements
even when the selected role names another Original. When no Original is
selected, it can promote retained availability into a compiler role.

This contradicts ReceiptSourceSelection::compiler_namespace_row, which
deliberately leaves RetainedCore without a selected compiler Original.
The existing positive retained-generation test asserts that distinction.

Start from the actual selected carrier and ordered issued edges. Retain its
payload custody through narrowing without broadening compiler namespace.
Only an explicit source/input issuer may choose a different role. The historical
reader's lost-custody symptom is not sufficient evidence that blanket role
promotion was necessary.

## Publication is one paired state transition

prepare_world_execution_selection validates latest-public selection after N,
but selection_for_promotion still starts from completed private selection.
The unchanged compiler branch preserves the latest World, while commit installs
the old runtime selection. A no-write A admitted before source-changing B is the
smallest useful discriminator. Fixing only the World branch was insufficient.

A simple replacement of the base with latest-public is also insufficient:
the current validation requires selected keys to exist in private custody,
which cannot authenticate legitimate public-only progress. The existing owner
must distinguish retained public selections from actual private transfers and
validate each against its owning evidence.

Separately, visible retained host output goes through mount_checked_host_input,
changes binding/visibility facts, and accepts its write journal without updating
the paired World association. Freeze refuses before progress classification.
The hidden-input route does refresh its association; it is not a substitute for
publishing a visible typed result. Use the actual host binding/interface issuer.

## Test acceptance needs correction

The six-case package cohort executes real ordinary compiler smoke paths.
It does not exercise a World segment item. The historical case restores real
products and issues separate singleton graphs, bypassing the new linkage reader.

The duplicate-envelope case checks only nonempty lists. The transitive case
need not find a competing child. The multi-ordinal case unions owners, so it
does not establish each ordinal's exact roots. The graph observation silently
stops at depth eight and omits exact edge identity. These limitations explain
why six passes can coexist with the current defects.

Preserve useful smoke coverage under honest names. Add minimal production
histories whose premises and exact chosen edges are asserted. A small test of
the actual stage/item join has higher value than more ordinary-path variations.

## What to retain; what should disappear

Retain W's segment reuse semantics and immutable token ownership; ordinary W
needs the same authenticated decision, not global relaxed Eq. Q2 correctly
uses census facts instead of full-payload equality. Q3 aggregates trace data
once inside the enabled branch. Q1's field/event bounds and asynchronous-exception
escape are useful. Q4 deletes the facade authority reconstruction.

Delete global HOME rematching, whole-HOME reprojection just to recover lost
choices, symbol/seal inherited search, and canonicalization whose only purpose
is making recreated environments look like one issuer. Retain payload byte
deduplication and genuine finite-graph conflict checks.

This is structural simplification, not a measured speedup. C's metadata-demand
observations remain hypotheses until their actual consumers and independent
semantics are established.

## Why the previous wave missed this

The concrete failure pattern is loss of an issuer relationship followed by
reconstruction from a wider inventory or a late final-state comparison.
The package contract itself excluded segment items while its shared consumer
required roots. The publication enum classified end states without recording
started work. Tests exercised nearby constructors and smoke behavior but not
these cross-owner joins.

Repair both mechanisms and detection: total paired constructors, owner-issued
progress, exact-edge traversal, and a small early joined history per seam.
More policy reminders or a larger catch-all test framework would not enforce
these relationships.

## Review of the revised assignments

The four context-loaded reviewers also checked the next-wave contracts and
assigned controls. Their corrections are incorporated: ordinary overlap versus
segment-delta duplication; unchanged A versus genuinely conflicting private/
public edits; legal duplicate row contents versus duplicate IDs; and real
issuer discovery before claiming competing child histories. True cold recovery
reports prior-machine heap bindings unavailable; preserving live host values
across machine loss would be a separate feature, not part of N.

This is acceptance of a bounded technical direction, not implementation or
runtime qualification. Remaining construction/semantics questions have explicit
escalation points in P/N rather than implied fallback permission.
