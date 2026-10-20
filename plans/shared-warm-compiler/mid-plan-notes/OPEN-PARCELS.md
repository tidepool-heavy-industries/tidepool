# Open parcels at the integration join

Code tip: `92fa518105`. Adversarial reviews are under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reviews/ADVERSARIAL-*.md`.
A parcel is merge-ready only when its review allows the merge *and* its counted
controls have been rerun on the join tip.

## Review outcomes

| Parcel | Verdict | Merge state |
|---|---|---|
| Batch A stale repairs | Conditional accept (test-only, no weakened assertions) | merged at `92fa518105`; joined run 11/11/11 (`09-*`) |
| Root E over-selection | Keep open / defer | not merged |
| Batch B repairs | Do not merge | not merged |
| Batch C repairs | Not merge-ready | not merged |
| Native carrier custody | Request changes | not merged |
| Reload follow-up | Block (target does not compile) | not merged |
| Second-cell evidence | Block on the evidence claim | telemetry commit reverted |

No *passing* Batch A assertion was weakened to match product behavior. Batch B
and Batch C contain such weakenings; that is why they stay out of the join even
though their authors reported green runs.

## Second-cell evidence: telemetry reverted

`e34183b852` emitted `compiled_off_checkout = true` unconditionally on every
certified install. Certified targets carry no compile provenance, so the field
could assert a false fact. `1b37c01049` reverts it.

After the revert,
`actor_host::call_timing_tests::second_cell_install_compiles_off_checkout` ran
1 selected / 1 executed / 0 passed / 1 failed, exit 1
(`final-join/logs/08-second-cell-case1-post-revert.log`). That is the honest
signal: the second cell's off-checkout compile is not observable through the
current telemetry. The case must not be counted green until a real provenance
field or a split-path-specific assertion exists.

The review also found the case-1 oracle tautological (an `any` over install
rows that bootstrap installs satisfy) and the 8 MiB stack widening test-only
but masking the `fast-dev` O0 frame. Case 2's stack-abort control is genuine.

### Case 3

Case 3 failed on the origin branch with
`SourceAdmission(MissingTransaction)` at `activation_input_tests.rs:5387`,
before any compiler input. `begin_private_execution` now requires an active
source compiler transaction (`persistent.rs:8512-8524`, introduced by
`e52d4072bc`/`3d56ffcc344`) while the module kept the 2026-10-04 fixture shape.
The join already contains `b5551a3436`, which repairs that shape, so the origin
failure is not evidence about the join. Rerun case 3 at the join tip before
drawing any second-cell conclusion.

## Product fixes

### Root E: first publication over-selects unrelated owners

- Branch: `fix/trunk-red-a` at `bfb8471722`.
- Claim: first issuance derives `ExactArtifactSelection` from the narrow
  closure computed by `published_artifact_roots` instead of retaining
  `graph.roots`. The offer filter is inline in `select_publication_roots`
  (not in `retain_issued_offer_custody`), and `for_compiler_roles` no longer
  treats `InterfaceOnly` as selecting a native row.
- Evidence on the branch: four target cases pass; `published_source` 7/8.
- Review: keep open / defer. A differential against current first-publication
  behavior is the required next evidence. Deferring root E leaves Batch A's
  case #11 red.

### Native carrier custody (b1 + b3)

- Branch: `fix/native-carrier-custody` at `a8b37dd1f3`.
- b1 chooses the carrier from the demanded group's issued requirements; b3
  derives owner choice and ambiguity from issued roots only.
- Review blockers: b1 is not proven unreachable — `native_availability.rs:396-427`
  still admits unauthorized groups and `:437-471` selects a sole compatible
  foreign candidate, so the negative control must cover a *single* foreign
  group. b3 contradicts the maintained test
  `continuation_namespace_preserves_roles_without_promoting_retained_bodies`
  (`certified_products.rs:12739`), which the branch fails. b2 belongs in
  `declaration_context.rs::normalize_issued_contributions`, not in
  `NativeOriginalRequirement`; b4 was not attempted.
- Merge guidance: do not merge as-is. Merging without b2/b4 is acceptable only
  after the b1 and b3 blockers are resolved, the full suite is no worse than
  baseline, and b2/b4 are explicitly quarantined as pre-existing red.

### Merge-path native group resolution (Batch B case 2)

- No branch. The failing assertion is
  `NativeGroupUnavailable { original_ordinal: 11 }` in
  `exact_group_availability`.
- Review: the reproduction runs through `merge_fixture_selections`, a
  `#[cfg(test)]` helper, not production
  `merge_compiler_inputs_inner` → `retain_merged_selection`. "Product defect"
  is therefore not established; a production-path repro or a fixture
  correction is required first.

### A1c: published introspection example

- No branch yet; investigation in progress.
- `actor_host::documentation_tests::published_introspection_example_returns_structured_declaration`
  is **red at the join tip** (`fc73e0e652`, code `92fa518105`): 1 selected /
  1 executed / 0 passed / 1 failed, exit 1
  (`final-join/logs/15-a1c-tip-fc73e0e652.log`). Earlier reds: `36827d80a7` and
  handlers `5eb2b3dbed`.
- The workbench fails before any Haskell helper runs:
  `ExactModuleIdentity { unit: "main", module: "Tidepool.Session.Lib.G4" }`
  requires unavailable
  `ExactModuleIdentity { unit: "main", module: "Tidepool.Effects.Core" }`
  through `NativeGroup { dependent_ordinal: 4, required_ordinal: 4174 }`.
  The Haskell probe script has never been observed to execute.
- Ordinals are fixture-local and unstable across bases (earlier observation:
  4192 vs 4205); do not treat them as identity.
- Do not use the concurrent scratch-tree trace: that investigation ran in
  `tidepool-front-door-rebase-20261019` (a 20-commit front-door stack, not one
  commit) with local `artifact_inventory.rs` and `native_availability.rs` edits,
  and fails as `InputNoCompatibleCandidate { candidate_count: 0 }` instead.
  Its tree-local finding is that the catalog does return one permitted support
  for `(G4, 4)`, which is then removed by the `IssuedChoices` exact-binding
  retain — pointing at an `ArtifactSelectionId` mismatch rather than a missing
  row. That hypothesis is **unmeasured on the join**; the tree's uncommitted
  edits change the failure shape. Notes:
  `reviews/A1C-INSTRUMENTED-TREE.md`. Its first next observation
  (`input_demand_decision`: `chosen` vs supported binding) is instrumented but
  was never run to completion.
- Open question: why `Tidepool.Effects.Core` is absent from the inventory.
  A link to the Batch A #8 capability gap (`aee5881d9c` removed the candidate
  native authority issuer) is plausible and unproven.
- Detail: `reviews/A1C-TIP-FINDING.md`; earlier forensic:
  `reviews/A1C-FORENSIC.md`.

## Test/oracle repairs

### Batch A stale repairs

- Branch: `fix/trunk-red-a-stale` at `3002d9cbb9` (base `630f6e5d35`, 4 files).
- Merged at `92fa518105`; joined 11-case rerun is 11/11/11.
- The author's two corrections to the batch report hold: #7 is a real
  carried-identity mismatch
  (`Mismatch("selected original is outside retained custody")` at
  `certified_products.rs:2656`), and #8 is a capability gap after
  `aee5881d9c` removed the candidate native authority issuer.
- Still open: #7 (product), #8 (capability), #9/#17/#18 (undetermined), the #5
  flake explanation, and a dedicated rerun of the 44-name ledger (the full
  toolchain run at `92fa518105` records 32 failures).

### Batch B repairs

- Branch: `fix/trunk-red-b` at `a67c933ed0` (tests only).
- Ten cases pass on the branch. Review: do not merge. Four *passing* repairs
  weaken assertions: case 8 drops the `graph_visits` bound, case 9 checks only
  subset/ordinal instead of exact selected group identity, case 10 measures
  `+1` across two different admissions, and case 11 no longer compares native
  group/custody sets. Case 7
  (`staged_native_histories_match_raw_fact_model`) stays red at step 43, slot
  0; the parent failed at a different step, so "same cause" is not established.

### Batch C repairs

- Branch: `fix/trunk-red-c` at `1c3eaa8ab4` (parent `0e66b8530f`).
- Seven cases pass on the branch; report at `reviews/trunk-red-c/REPORT.md`.
- Review: not merge-ready. The typed-cause rationale is wrong:
  `CertificationError::Mismatch` has 385 occurrences repo-wide, 378 in
  `tidepool/toolchain/src`, and 145 immediate string-literal constructions; an
  *additive* variant leaves the transport untouched (about one construction
  plus a few match arms). The custody cases cannot distinguish missing custody
  from a misbound original — the same string arises from Batch A's still-red
  case. The `checked_cell` ambiguity control compares `NativeGroupKey`, which
  erases `NativeGroupIssuer.selection`. `PublicationNativeAuthority::bind` is a
  production change qualified by one focused case only.

## Reload follow-up

- Separate lineage `1ce33b0a34` → `9c4fae4117` → `77e0f50f7a`, not on top of
  the merged `eadc18a6fe`; absent from the join.
- Refuses unsealed preview installation and spent-admission reuse; fixes cached
  renderer binding ownership.
- Review: BLOCK. The assigned unit-test target does not compile at
  `77e0f50f7a`; the prescribed control selected/executed 0/0 with exit 3
  (`reviews/ADVERSARIAL-RELOAD-control.log`). The author's passing controls came
  from a different lineage (`bd3143ee26`), and the
  revoked-original-A-before-consumption case remains untested.

## Deferred decisions

- `CertificationError::Mismatch` transport: a typed cause is a smaller change
  than the Batch C report claims, but it is still an additive variant plus
  match arms; schedule it when a parcel needs to distinguish the cause
  programmatically.
- `BrowseBounded`: the coherence audit finds it dead across runtime,
  `tidepool-extract-cmd` and Haskell, retained under a "non-compiling route"
  rationale that no longer holds. Delete it with the next inspection cleanup.
- `ViewIdentity`/`view_identity`: dead public API whose adjacent doc still names
  it as the staleness owner. Remove or re-document.
- Frozen bundle qualification and a packaged end-to-end persona run: required
  before any release statement.
- The 32 toolchain failures at `92fa518105` (2 stale, 12 product-suspect, 18
  undetermined) are catalogued in `final-join/TOOLCHAIN-SWEEP.md`.
