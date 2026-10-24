# Import composition and retained ownership closeout

Accepted implementation plan, 2026-10-24. Base code `35066c2115040ce9cf462d8ade764cb2ef84a4fc`, base documentation `b54861ea5cdbad7d3966257e29c42da4ded50f42`.
Integration owner: root; branch `integration/g2-domain-runtime-20261023`.
Evidence: `/srv/swarm/evidence/inanna/g2-import-ownership-closeout-20261024/`.
Implemented code candidate: `75a0aa0fed3e30745ff43053d9ed5ea1a0f7cd72`.
The three joined history cases now pass; see
[review checkpoint](g2-import-ownership-review-20261024.md) and
`joined-acceptance.json` in the evidence directory. G2/G3 remain unqualified.

## A: one chronological declaration-import composition

Owner: Sol import worker. Own `bridge/haskell` production changes and affected tests/build-input registration.

- Consolidate previous verified declaration plus current-cell declaration inventories, in issued chronological order, into one `refineProgramDeclarationImports` call. Preserve `[DeclarationExport]` head/child groups. Add a pure projection from `PlannedDeclarationInventory`; do not fabricate a planned inventory for the facade.
- Retain exact HPT owner, fingerprint, complete export inventory checks; reject duplicate input owners. Facades legitimately reexport Names belonging to underlying Originals.
- Check the unique direct unqualified/no-alias/no-package/non-SOURCE wildcard premise against immutable captured syntax. Bind its exact occurrence root to the checked owner; track the corresponding live unqualified descendant. Restricted authored imports remain restricted. A different same-module occurrence cannot substitute.
- Make narrowed/qualified IDs contain a nonrecursive captured-or-Prelude root. Derive lineage from identity. First narrowing creates one narrowed occurrence and one original-selection qualified clone; later narrowing updates in place. Qualified narrowing refuses. Unchanged selections remain unchanged.
- Synthetic Prelude follows that same lifecycle. Final admission checks its canonical root, including both descendants, against synthetic requested name/qualifier/owner.
- Use one lexical-key policy in shadowing and winner selection, including record-field parents. Generated parent-bearing field selections match exact OccName; authored bare field/type/pattern imports retain GHC semantics.
- Delete separate `refineOriginalDeclarationImports` and superseded transformation wrappers, migrating singleton tests to the production composition.

Tests: captured wildcard after local narrowing; repeated narrowing identity/clone count/monotonicity; repeated Prelude narrowing; old facade plus new Originals, grouped constructor precedence and qualified historical reads; DuplicateRecordFields across multiple passes; explicit/hiding, duplicate/missing, aliased/package/SOURCE/qualified-only substitution refusals; legal restricted alias beside direct import. Inspect other `selectedImportNames` consumers (Display and quasiquote/provider selection).

The former stage-9 G5 wildcard failure is repaired. Joined execution at
`1519173f26714b617305ab3497c5cdaa0c6b7422` compiles that declaration and reaches
the next segment. Its demand renamer still consumes raw G3/G5 imports before
the final frontend applies chronological refinement, producing an ambiguous
`historyCaptured`. Both the deterministic repeat and a distinct-value history
reach this same boundary; their cold histories succeed. Retained diagnostics:
`joined-1519-history-two-retry/{4af84036685f91b40a3f31a583d4187c031f56059edf1ad9a601de7a48b2e33d,af7aeb8565ba408173b32b42fd7621d87e6da16cedb4a213dbe50975afcad7d6}/artifacts/compiler-transactions/`.

The implemented follow-up derives the demand import view from the actual matched
generated driver after prefix/import/operand restoration. Reuse the local
declaration and chronological import rules. Project only imports onto the
original no-quasiquote/root-checked demand declarations, with the same native
annotations. Keep raw captured imports for source qualification. Do not call
the complete final transform from demand: completed value aliases depend on
demand, and typed-segment lowering/inspection normalization belong to final
checking. Reject mismatched captured sources or refined driver ownership.

## B: selected values consume exact target-issued slot evidence

Owner: Sol retained worker. Own `checked_cell.rs`, `linked_native.rs`, affected runtime authorization/installation/tests and the stage-sealing portion of `declaration_context.rs` (WorldProgramSegment/SealedWorldStage, approximately lines 12k–13k). Coordinate any other shared-file edit with C.

- Replace leaf-only `cell_stage_retained_input` with an operation consuming an opaque target-issued slot witness. Privately retain receiving target and Current/Prior classification so callers cannot relabel a witness via public enum reconstruction.
- Preserve the exact stage-to-own-physical-product association at `WorldProgramSegment::seal`, where stage/source and `originals.captured_stage()` are together. Carry a thin borrowed/shared owner through SealedWorldStage/admit_world_cell into ExactCheckedCell. Resolve exact NativeGroupBinding through its own completed offer and actual physical issuance. No module-name inference, broad inventory matching, new cache, or declaration-certificate reconstruction.
- CurrentStage accepts explicit receiving-stage input or a sealed strictly earlier item in that same stage. PriorProgramStage requires an actual earlier issuer association AND explicit receiving-stage input; never EarlierItem fallback. HistoricalInput keeps exact NativeOriginal terminal plus Admitted evidence.
- PriorStage selection joins the selected producer item to the actual same-cell sealed checked row, symbol, generation, binder ID, scope and type artifact. Runtime Produced evidence requires Arc identity with that row.
- `producer` is an item index; `consumer` is a stage first-item ordinal. Preserve authenticated carried consumer provenance: consumer can precede the current stage. Do not require producer.stage < original demanding stage: an executable group may capture a value produced within that original stage and later selected explicitly.
- No reconstruction of an earlier runtime prefix is required. Exact physical code/slot provenance and the receiving execution's authenticated value selection prove separate facts. Preserve admission/item/target/requirement, lexical lease, liveness, representation, signature, evaluatedness and anchored-descendant checks through installation.
- Delete the old leaf-only authorization entry point. Do not introduce persisted authority or a second runtime value registry.

Tests: existing same-stage result and substitutions; exact PriorStage row/producer substitution; actual prior declaration capture; three-plus-stage NOINLINE and inline histories; prior executable reuse with explicit later selection; unselected old/current-stage values; wrong issuer stage/item/target/slot/admission; historical terminal negatives; entry and anchored descendants. Tests must assert which real production branch they exercised.

## C: preserve issued native rows through materialization

Owner: Sol row worker. Own materialization/collector/certification regions of `declaration_context.rs` (approximately 7k–8k and 17k–18k), `certified_products.rs`, related artifact-inventory consumers and tests. Do not modify B's stage-sealing region without coordination.

- `CertifiedNativeGroupBaseline::Rows` borrows actual issued row Arcs. Feed their products into physical membership, their complete rows into inheritance and their ordered imports into link validation. Preserve actual existing row Arcs; new aliases remain distinct bindings.
- Introduce one selected-issuance comparison: physical same_issuance plus ordered selected-import equality. Keep physical-only comparison distinct.
- Use one private fallible native-row collector for selected_group_refs and select_custody_facts. Filter requested bindings first; refuse competing issuance for one requested binding, including before frontier reduction. Use complete comparison in inherited-row conflict checks and frontier coverage.
- Preserve physical file/ArtifactId custody precedence and unselected alternatives. Propagate Result through existing fallible production callers instead of silently dropping conflicts.
- Replace two fixture CBOR witness surgeries with typed ReceiptImportOwner::OriginalBinding through issue_home_certification_with_validation. Preserve request-derived exact pairs/binders, real physical completion, Fresh parent and nominal-only refusal.
- Move already-private cfg(test) available_original_source_map into tests. Reduce issue_source_overlay_groups to one ordinary issuance from selected keys. Preserve changed-pending-import refusal at its actual certification owner (`selected_group_validation_keeps_exact_bindings_across_equal_owner_custody`); preserve foreign issued-owner overlay refusal. Do not add a test-only authority constructor to preserve an artificial helper signature.

Tests: inherited row Arc identity (existing test only checks physical issuance); nonempty ordered imports through materialize/detach/request; same binding independently issued twice refuses in both parent orders and custody reduction; repeated identical issuance/overlapping parents succeeds; unselected alternatives and physical-file precedence remain valid; wrong row/binding refuses. Derive expectations independently from the collector under test.

## Integration and acceptance

Workers use isolated checkouts, real source revisions, pinned just/Buck entrypoints and per-checkout output leases. Compile affected production and cfg(test) targets early; retain exact commands/counts/outcomes. Ordinary commands use swarm-command; builds use swarm-build with admission and local-only policy. No ad hoc println debugging. Preserve meaningful diagnostic records separately from bounded presentation.

Root reviews actual diff/consumers and integrates coherent commits incrementally. Import repair should be integrated early enough to rerun the maintained deterministic history while the independent Rust work finishes. Integration checkpoints are not qualification.

Run `session::turn::scaling_tests::typed_segment_tests::deterministic_capture_history_matches_ghc_cold_warm_and_recovery`, then bounded generated capture histories via the owning counted runner. Discover exact selectors rather than using Argon's illustrative `_one` names. Run regional controls, translation/fixture checks as applicable, and audit old entrypoints/callers for deletion.

Return consolidated candidate and evidence to external Argon for final adversarial acceptance. Subsequent G2 remains the existing contract: ordinary notebook/discovery/reload/lifecycle controls, matched frozen bundle, selected Persona spec/tools/rendered memory, bounded real DeepSeek smoke. G3's broader force/fork/recovery/M1–M3/performance obligations remain separate.

## Progress

- [x] A implemented, compiled and focused controls executed (six named cases; `imports/summary.json`)
- [x] B implemented, compiled and focused controls executed (physical control, seven runtime controls, final prior-stage rerun; `slots/evidence.json`)
- [x] C implemented, compiled and focused controls executed (nine controls plus strengthened ordered-import control; `rows/evidence.json`)
- [x] Joined deterministic, distinct-value and bounded generated histories executed and passed
- [x] Integrated deletion/consumer review and Argon handoff prepared (external acceptance pending)
- [ ] Existing G2 bundle and Persona acceptance

Selected joined fixture cohorts passed at `5650c5773b`: one recovered-base
program and eight user-type programs; `rows/joined-fixture-evidence.json`.
The first joined history attempt built successfully but failed during compiler
service startup on a malformed Nix root-query response. The retry reached the
actual notebook failure described above; retain those as separate outcomes.
The later import-projection repair passes the warm histories, rejection and
recovery controls. No frozen bundle or Persona acceptance is claimed.

B's same-stage and prior-declaration controls pass separately. They do not
establish reuse of an executable group whose capture was produced in that
group's original stage. A bounded reachability trace found thin value
interfaces and excluded executable scaffold owners prevent the obvious
ordinary notebook example from exercising that branch. This combined positive
and the independent PriorProgram-without-receiver-selection refusal remain
unverified, not passing claims; see
`slots/prior-executable-same-stage-reachability.json`. The private origin and
CurrentStage-only EarlierItem fallback remain source-reviewed constraints.
