# Open parcels at the integration join

Adversarial reviews are recorded under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reviews/ADVERSARIAL-*.md`.
A parcel is merge-ready only when its review allows the merge *and* its counted
controls have been rerun on the join tip. Review outcomes are summarized first;
the parcel detail follows.

## Review outcomes

| Parcel | Verdict | Merge state |
|---|---|---|
| Batch A stale repairs | Conditional accept (test-only, no weakened assertions) | merged at `92fa518105`; joined 11-case rerun is `final-join/logs/09-*` |
| Root E over-selection | Keep open / defer | not merged |
| Batch B repairs | Do not merge | not merged |
| Batch C repairs | Not merge-ready | not merged |
| Native carrier custody | Request changes | not merged |
| Reload follow-up | Block (target does not compile) | not merged |
| Second-cell evidence | Block on the evidence claim | telemetry commit reverted; see below |

The review found no case in which a *passing* Batch A assertion was weakened to
match product behavior. Batch B and Batch C contain such weakenings; that is
why they stay out of the join even though their authors reported green runs.

## Second-cell evidence: telemetry reverted

`e34183b852` emitted `compiled_off_checkout = true` unconditionally on every
certified install. Certified targets carry no compile provenance, so the field
could assert a false fact. The join reverts that commit (`1b37c01049`).

After the revert, `actor_host::call_timing_tests::second_cell_install_compiles_off_checkout`
fails 0 executed / 1 selected / 0 passed (exit 1,
`final-join/logs/08-second-cell-case1-post-revert.log`). That is the honest
signal: the second cell's off-checkout compile is not observable through the
current telemetry. The case must not be counted green until a real provenance
field or a split-path-specific assertion exists.

The review also found the case-1 oracle tautological (`any` install row, which
bootstrap installs satisfy) and the 8 MiB stack widening test-only but masking
the `fast-dev` O0 frame. Case 2's stack-abort control is genuine; case 3 is a
stale fixture module (`SourceAdmission(MissingTransaction)`), not a missing
declared input.

## Product fixes

### Root E: first publication over-selects unrelated owners

- Branch: `fix/trunk-red-a` at `bfb8471722`.
- Claim: first issuance derives `ExactArtifactSelection` from the narrow closure
  computed by `published_artifact_roots` instead of retaining `graph.roots`.
  Two collateral producers also changed: `retain_issued_offer_custody` filters
  offers to the selected originals, and `for_compiler_roles` no longer treats
  `InterfaceOnly` as selecting a native row.
- Evidence on the branch: four target cases pass; `published_source` 7/8.
- Risk: this changes selection semantics for every first publication. It must
  not be merged on branch-local evidence alone.
- Blocks: any claim that warm publication selects only the demanded closure.
- Review: keep open / defer. Deferring root E leaves Batch A's case #11 red. A
  differential against current first-publication behavior is the required next
  evidence, not another branch-local pass.

### Native carrier custody (b1 + b3)

- Branch: `fix/native-carrier-custody` at `a8b37dd1f3`.
- b1 chooses the carrier from the demanded group's issued requirements; b3
  derives owner choice and ambiguity from issued roots only.
- Five controls pass on the branch (1/1 each).
- Open inside the parcel: b2 (multi-group/one-carrier availability) still
  refuses with `BindingAmbiguity`; b4 was not attempted. Merging b1+b3 is safe
  only if b2 is not relied on.
- Review blockers: b1 is not proven unreachable — `native_availability.rs:396-427`
  still admits unauthorized groups and `:437-471` selects a sole compatible
  foreign candidate, so the negative control must cover a *single* foreign group.
  b3 contradicts the maintained test
  `continuation_namespace_preserves_roles_without_promoting_retained_bodies`
  (`certified_products.rs:12739`), which the branch fails. b2 belongs in
  `declaration_context.rs::normalize_issued_contributions`, not in
  `NativeOriginalRequirement`.

### Merge-path native group resolution (Batch B case 2)

- No branch. `exact_group_availability` fails with
  `NativeGroupUnavailable { original_ordinal: 11 }`: a cross-inventory merge
  picks island 1's artifact binding while the ordinal-11 group carries island
  0's, and `resolve_group_binding` (`artifact_inventory.rs:6004`) requires the
  namespace carrier.
- Blocks: claims that the merge path resolves native groups correctly.

### A1c: published introspection example

- No branch yet; investigation in progress.
- `actor_host::documentation_tests::published_introspection_example_returns_structured_declaration`
  selects and executes one case, then fails: the example returns `rejected`
  because `Tidepool.Session.Lib.G4` requires `Tidepool.Effects.Core`, which is
  unavailable (ordinal 4192 vs 4205 in the retained observation).
- Earlier forensic at trunk is `reviews/A1C-FORENSIC.md`; it must be rechecked
  on the join tip after any merge.

## Test/oracle repairs (no production behavior)

### Batch A stale repairs

- Branch: `fix/trunk-red-a-stale` at `3002d9cbb9` (base `630f6e5d35`, 4 files).
- Eleven cases pass on the branch. The author flags two corrections to the
  batch report: #7 is a real carried-identity mismatch
  (`Mismatch("selected original is outside retained custody")` at
  `certified_products.rs:2656`), and #8 is a capability gap after
  `aee5881d9c` removed the candidate native authority issuer.
- Merged at `92fa518105`. The merge conflict in `declaration_context.rs` kept
  the join's newer checked-issuance block (it already performs the
  recover/issue/retain sequence with an extra `is_full` guard and a negative
  coverage control); the branch's older helper call was dropped there.
- Still open after merge: #7 (product), #8 (capability), #9/#17/#18
  (undetermined), and the reviewer's request to explain the #5 flake and run a
  module-scoped toolchain sweep on the joined tip.

### Batch B repairs

- Branch: `fix/trunk-red-b` at `a67c933ed0` (tests only).
- Ten cases pass. Case 2 remains a product defect (above). Case 7
  (`staged_native_histories_match_raw_fact_model`) remains an oracle gap: the
  projection re-issues roots under a fresh `ArtifactSelectionId` while keeping
  the parent binding as custody, and the model omits the fresh binding. It also
  failed at parent `36827d80a7`; it must not be counted green.
- Review: do not merge. Case 2's repro runs through `merge_fixture_selections`,
  a `#[cfg(test)]` helper, not production
  `merge_compiler_inputs_inner` → `retain_merged_selection`, so "product
  defect" is not established. Four *passing* repairs weaken assertions: case 8
  drops the `graph_visits` bound, case 9 checks only subset/ordinal instead of
  exact selected group identity, case 10 measures `+1` across two different
  admissions, and case 11 no longer compares native group/custody sets.

### Batch C repairs

- Branch: `fix/trunk-red-c` at `1c3eaa8ab4` (parent `0e66b8530f`).
- Seven cases repaired on the branch; the author's thread died before writing
  a lane file. Report: `reviews/trunk-red-c/REPORT.md`.
- Deliberate decision recorded there: the two custody cases assert the existing
  `CertificationError::Mismatch("selected original is outside retained custody")`
  string rather than adding a typed variant, because `Mismatch(&'static str)` is
  constructed at roughly 385 sites.
- Review: not merge-ready. The typed-cause claim is wrong: `Mismatch` has 378
  occurrences in `tidepool/toolchain/src`, and an *additive* variant leaves the
  transport untouched (about one construction plus a few match arms). The
  custody cases cannot distinguish missing custody from a misbound original —
  the same string arises from Batch A's still-red case. The `checked_cell`
  ambiguity control compares `NativeGroupKey`, which erases
  `NativeGroupIssuer.selection`. `PublicationNativeAuthority::bind` is a
  production change qualified by one focused case only.

## Reload follow-up

- Branch: `feature/shared-warm/reload` at `77e0f50f7a` (two commits on top of
  the merged `eadc18a6fe`).
- Refuses unsealed preview installation and spent-admission reuse; fixes cached
  renderer binding ownership.
- Not executed on the join tip.
- Review: BLOCK. The assigned unit-test target does not compile at
  `77e0f50f7a`; the prescribed control selected/executed 0/0 with exit 3
  (`reviews/ADVERSARIAL-RELOAD-control.log`). The author's passing controls came
  from a different lineage (`bd3143ee26`), and the
  revoked-original-A-before-consumption case remains untested on this branch.

## Second-cell evidence

- Origin branch: `wip/warm/second-cell-evidence` at `921811d95c`; its two
  commits are cherry-picked into the join.
- Case 3 (`activation_input_tests`) fails before compiler input with
  `SourceAdmission(MissingTransaction)` at `activation_input_tests.rs:5387`.
  `begin_private_execution` now requires an active source compiler transaction
  (`persistent.rs:8512-8524`, introduced by `e52d4072bc`/`3d56ffcc344`), while
  the module retains the 2026-10-04 fixture shape. Classification: stale test
  module, not a missing declared input.

## Deferred decisions

- `CertificationError::Mismatch` transport: a typed cause would touch roughly
  385 construction sites. Deferred until a parcel needs to distinguish the
  cause programmatically.
- `BrowseBounded`: kept as a binding-only budget surface; no new effect verb.
- Frozen bundle qualification: required before any release claim, not attempted
  at this tip.
- Trunk-red ledger: 44 reds were catalogued at `0e66b8530f` (Batch A 21, B 16,
  C 7). Repairs cover the stale subset; the product reds above are the residue.
