# G2 retained ownership: Argon review checkpoint

Integration checkout: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
Branch: `integration/g2-domain-runtime-20261023`.
Code candidate: `35066c2115040ce9cf462d8ade764cb2ef84a4fc`.
Accepted packet: SHA256 `a774cd044a2f907f62a10ed72fee32e624c66d313cb0142d99b62261d44678e2`.
Review base: `a3edd881fbc3fa73eb70c03df3423e678313b6e4` (code `dc34094d5e`).
Evidence directory: `/srv/swarm/evidence/inanna/g2-argon-implementation-20261024/`.

The user selected Argon for this review. G2/G3 remain unqualified. No bundle,
main merge or Persona acceptance is claimed. The final runtime test target
compiles at `35066c2115`; both bounded ownership controls pass (2 executed,
0 failed). The checkpoints below
distinguish executed checks from remaining qualification work. Review the pinned commits
rather than assuming current HEAD has already executed every check.

## What changed and why

The four planned parcels are integrated: explicit HOME provenance and exact
slot witnesses; runtime authorization bound to item/admission/target/slot;
single owned Haskell interface hydration; producer-sealed per-group metadata.
Superseded boolean HOME provenance, RetainedNativeOrigin, the raw installed
interface overload, and the exported captured-interface injection path were
removed. Legitimate `is_produced` bookkeeping remains; it no longer decides
whether a retained slot is authorized.

Joined execution exposed three further ownership losses, now repaired:

1. Consuming target snapshots inherited executable roots belonging to earlier
   targets. Target construction now selects exact roots and retains the other
   exact nodes only as custody. General joins and complete-prefix checks remain
   strict. `TargetNativeInputs` is required at target certification.
2. Split native demands recertified a group without its prior issued row.
   The request now lends its actual previous acquisitions to certification.
   No content interning or relaxed merge equality substitutes for issuance.
3. Product-source membership flattened selected groups into pending payloads.
   `ProductInputGroups::{Exact,World}` now carries the actual selected-row owner
   through the overlay, preserving exact carrier pairs, children and imports.

The deterministic history then exposed a missing category in the accepted plan:
**an earlier item inside the same compiler stage**. At `eeb3835adc`, runtime
item 3/stage 1 demanded `Val.G2.historyRead0`, generation 2, while the incoming
stage selection was empty. That is a legitimate same-stage producer, already
represented by the sealed item prefix, rather than a missing cross-stage input.

Root's `6ceb151470` (borrow fix `e57abc48d3`) preserves the producer item and
actual checked-row Arc in the existing sealed prefix. The receiving compiled
item issues `CellStageRetainedInput::{StageSelected,EarlierItem}` only after
exact native-demand membership, including the artifact identity. EarlierItem
also requires the same cell, same stage and strictly earlier item order.
Runtime `853f00b1c1` consumes this proof with actual Produced evidence, matching
row Arc, binder ID and defining scope. All acquired-slot, admission, lexical
lease, liveness, representation, evaluation and signature checks remain.

## Primary review requests

Start with request 2 if the immediate goal is unblocking the remaining history;
review request 1 independently before accepting the new ownership extension.
The numbered order is not a dependency between those investigations.

### 1. Challenge the same-stage proof extension

Read `checked_cell.rs` (`CheckedSettledValues`, `select_settled_values`,
`cell_stage_retained_input`) and runtime `admission/scoped_inputs.rs`, then follow
the authorization through `prepared/closed.rs` and installation in `persistent.rs`.

- Does exact native demand intersect the correct sealed binder/producer owner?
- Can stage selection accidentally become a fallback for an unselected
  cross-stage or historical value? Can a future item or different cell enter?
- Does the authorization retain all associations through slot installation,
  including anchored descendants and the entry?
- Are compiler completion and runtime completion still separate?
- Does storing producer/row references introduce unnecessary retention or
  duplicate ownership? No new cache or alternate value ledger was introduced.

Do not treat `validate_settled_native_bindings(actual_singleton)` as a positive
membership proof: its aggregate validation intentionally skips requirements not
owned by its rows. The new issuer performs positive exact membership.

### 2. Resolve the warm declaration-import frontier

At `0b4c0cf5d2`, the deterministic test completes its cold execution, verifies
the expected `[1,0,1,0]` trace and publication-census assertion, then fails on
the second execution in the same session at `typed_segment_tests.rs:1245`:

```text
user error (planned declaration requires one direct original wildcard import)
```

This is a compile failure in the warm request, not a retained-slot refusal.
The required warm/rejection/recovery sequence has not completed.

Here "warm" means the same `SemanticSession` after the first successful cell's
publication. The second cell repeats the same source, including declaration and
binding names. This changes both compiler reuse conditions and the notebook's
visible predecessor history; the current experiment does **not** isolate a
compiler-cache defect from import/shadowing composition. Review
`tidepool/runtime/src/session/typed_segment_tests.rs::exercise_capture_history`
and the source generator, rather than inferring causality from the test label.

Exact failing transaction:

```text
/srv/swarm/evidence/inanna/g2-argon-implementation-20261024/integrated-0b4c-deterministic-capture-history-one/4af84036685f91b40a3f31a583d4187c031f56059edf1ad9a601de7a48b2e33d/artifacts/compiler-transactions/.tmpXP7zwu
```

Read `stage-9/Tidepool.Session.Lib.G5.hs`, `compiler.stderr`, and
`consumed-sources.json`. The latter reports complete source capture with no
issues; the outer runner nevertheless reports incomplete overall diagnostics.
Do not conflate those two completeness claims.

The raw generated source contains wildcard imports of both Lib.G3 and Lib.G4,
then declares `historyCaptured = historyValue4`. Two imports are not themselves
the refusal: `refineImports` checks each exact owner separately and reaches the
branch where its single direct import has a nonempty import-list field.

Source-grounded leading explanation, not yet a controlled repair result:

- `GhcPipeline.transformFor OriginalDeclarationCompile` first calls
  `refineParsedDeclarationImports`, which narrows unqualified imports to avoid
  names authored by this declaration.
- The World `pvTransformParsed` then calls `refineOriginalDeclarationImports`
  for the admitted declaration and `transformProgramDeclarationImports` for
  planned originals.
- `CheckedPrefixImports.refineImports` checks the current transformed AST for
  the wildcard premise. A legitimate earlier narrowing can have consumed it.
- `ImportSyntax.Internal.RefinedImportSyntax` already retains original captured
  syntax plus occurrence/refinement lineage. Investigate consuming that owner
  or composing refinements, instead of testing a transformed node as if raw.

Please decide the smallest sound owner boundary. Do not simply delete the
wildcard check or restore wildcard imports: authored selections, lexical
shadowing and qualified historical access must remain intact. Seek a minimized
two-cell positive with the same shadowing, paired with genuinely restricted,
aliased or foreign imports that must not gain access.

### Root-cause discriminators and requested output

Verify which exact owner and occurrence reaches the refusal before choosing a
repair. The source trace above does not establish whether local-name refinement,
the admitted-original pass, or a preceding planned-original pass first narrowed
that occurrence. Also inspect whether owner lists overlap and whether any
wrapper replaces the captured syntax/lineage with a transformed AST.

Useful starting points at the code candidate:

- `bridge/haskell/src/Tidepool/CheckedPrefixImports.hs:174` and `:280`:
  original/program refinement and the exact refusal branch.
- `bridge/haskell/src/Tidepool/GhcPipeline.hs:3130` and `:4337`:
  composition of parsed, admitted-original and planned-original transforms.
- `bridge/haskell/src/Tidepool/ImportSyntax/Internal.hs`:
  captured occurrence identity, `ImportLineage`, `mapRefinedImportsM`, and
  `RefinedImportSyntax`'s retained source association.
- `bridge/haskell/src/Tidepool/PlannedDeclaration.hs:285`:
  `transformProgramDeclarationImports` projects exact owner/interface/export
  inventories into `refineProgramDeclarationImports`.

Please return:

1. A verified cause with the first stage that violates the contract, or a short
   competing-hypothesis table with the cheapest distinguishing observation.
2. The proposed owning abstraction and invariants. Prefer one exact import
   occurrence with composable narrowing to reconstructing source intent from
   the current AST or carrying an unchecked "already processed" flag.
3. A bounded implementation plan, old entry points to delete or restrict, and
   affected consumers. Identify any semantic change separately from a repair.
4. A discriminating positive/refusal matrix: repeated-cell shadowing, genuine
   restricted/aliased/package-qualified imports, qualified historical access,
   and the joined cold → repeat → rejection → recovery history. Distinguish
   source-reviewed guarantees from actually executed cases.

Use normal opt-in diagnostics if extra observations are required. Do not make
the test pass by deleting the wildcard guard, authorizing by module name alone,
or broadening a restricted import. A smaller refactor that consumes original
evidence is preferable to a new parallel import framework.

### 3. Adversarial completeness and test-construction review

Check actual production consumers and refusal controls, not only counts. In
particular:

- Original metadata carries no execution permission. The nominal-only test
  stays separate from the issued-graph overlay positive. The positive earns its
  child pair through request-owned physical acquisition and target selection.
- The new overlay fixture initially guessed a binder, then used an unrelated
  inventory, then mislabeled a new parent as RetainedCore. Each refusal was
  correct. The repaired test derives facts from actual owners and asserts its
  premises before exercising the overlay. Consider whether existing fixture
  construction can express these contracts more directly without an authority
  factory or a new test-only bypass.
- Search joined consumers for remaining flattening of issued rows into payloads,
  boolean provenance, and repeated certification without the prior owner.
- Distinguish source-reviewed invariants from executed refusal controls. The
  worker evidence reports explicit gaps; do not infer exhaustive coverage.

## Executed checkpoints

These are separate batches, not additive distinct-test totals. Full commands,
counts, worker/binary hashes, cleanup and source OIDs are in each checkpoint.

| Commit | Evidence |
|---|---|
| `025a5fac47` | Six targets compiled. Three Haskell owner controls, historical capture, INLINE/SPECIALIZE, historical runtime refusal controls and two toolchain cases pass. Thirteen distinct cases in this batch: eight pass/five fail. One separate startup refusal executed zero tests and is retained independently. |
| `f507e3ba76` | Inherited effect history and strengthened resolver/installation controls pass; two other cases fail. |
| `85507b78e6` | Six of seven cases pass, including real class-default and transitive capture histories. New executable fixture premise fails. |
| `794d3efd01` | Both split-demand orderings and real sibling history pass; two remaining failures retained. |
| `9cd06f6c0a` | Complete-counter reader and original metadata/cancellation history pass. |
| `eeb3835adc` | Reader passes; deterministic refusal precisely identifies missing same-stage input case. |
| `0b4c0cf5d2` | Three targets compile. Four Haskell counter controls pass. Deterministic test advances through cold execution then fails during warm declaration compilation; direct overlay fixture still fails before its intended boundary. |
| `a59ac11a36` | Toolchain cfg compiles; corrected issued-graph overlay positive and its exact-owner guards pass. |
| `9ff6594267` | New tests and fixture registration; cfg compilation caught a fixture-wrapper method used on the real ResidentSession. Zero tests executed. Generator checks pass 61/61. |
| `987e55258e` | Runtime cfg compilation refused private `ResidentSession.state` access in the new test. Zero tests executed. |
| `35066c2115` | Runtime cfg compiles (36.3 s). Both exact ownership controls pass: same-stage producer/refusal coverage and historical-terminal/admitted-value coverage. Two executed, zero failed; process and compiler cleanup confirmed. |

Final batch: `integrated-3506-native-authorization-two-summary.json` under the
evidence directory. It records immutable source, complete command, native and
worker hashes, and one actual execution per case. Both reports still mark overall
diagnostic evidence incomplete; successful assertions and confirmed cleanup do
not establish full diagnostic capture. No joined warm rerun was performed after
the test-only caller corrections.

Selected counter evidence is now read from the complete diagnostic file before
bounded display. This fixed a real false failure and nearby false-pass hazards.
Four adjacent counter consumers pass unchanged semantic assertions. Some raw
phase-marker and frontend-text predicates remain bounded: see
`metadata-capture-audit.md`; this is not a claim that all observation paths were
migrated. Test observation bounds are not production compiler limits.

## Delivery and review limits

The validator checkout is
`/srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020`, owned by
`validation_resume`. Both final cases have settled; the checkout is clean and
idle, and its pin is released. `integrated-3506-checkpoint.json` records the
terminal state. Implementation workers are stood down for Argon review.

Existing public/main and bundled acceptance have not been rerun. No timing claim
follows from these checks. The next decision belongs to review of this integrated
candidate and the warm-import boundary; do not launch an unrelated rewrite wave.
