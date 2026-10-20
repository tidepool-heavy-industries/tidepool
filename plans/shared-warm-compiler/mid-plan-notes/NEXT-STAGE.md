# Next stage: reviewer brief

This is a seed prompt for a peer++ reviewer taking over the shared-warm
campaign. Replace the OID if the branch has advanced; only the documentation
commits should follow `92fa518105`.

## Read in this order

1. `plans/shared-warm-compiler/README.md` and `STATUS.md` for the campaign
   boundary and history. Both now open with a current-state banner: everything
   below the banner is the earlier backend handoff, with its own checkout,
   branch and product OIDs.
2. `plans/shared-warm-compiler/mid-plan-notes/MILESTONE.md` for what landed and
   what was executed, with the exact OIDs.
3. `plans/shared-warm-compiler/mid-plan-notes/OPEN-PARCELS.md` for unmerged
   work, review verdicts and known defects.
4. `plans/compiler-query-effects.md` for the five-verb decision.
5. The adversarial reviews and audits under
   `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`:
   `reviews/ADVERSARIAL-*.md`, `final-join/TOOLCHAIN-SWEEP.md`,
   `final-join/COHERENCE-AUDIT.md`, `final-join/SURFACE-AUDIT.md`,
   `final-join/ACCEPTANCE-MATRIX-75476fb97b.md`,
   `final-join/MERGE-PREFLIGHT-NEXT.md`, `reviews/READONLY-FOLLOWUP.md`.

## State you are inheriting

- Branch `integration/compiler-query-warm-final`; code tip `92fa518105`,
  documentation tip `fc73e0e652`.
- It builds. Three distinct selectors pass in six executions: query dispatch
  (`04`, `10`, and `14` at the docs tip), persona successor (`05`, `11`), and
  the front-door prepared fixture (`07` at `a4aa30a0a2`, `14` at the docs tip).
  The schema check (`03`) is analysis-time and predates the front-door merge; it
  is not a counted case.
- The full `tidepool-toolchain` suite at `75476fb97b` records 1054 passed /
  32 failed / 30 ignored (exit 1), better than the trunk baseline of
  1029/44/30 but not green.
- `A1c` is red at the tip: the workbench refuses with
  `ExactModuleIdentity { Tidepool.Session.Lib.G4 } requires unavailable
  ExactModuleIdentity { Tidepool.Effects.Core } through NativeGroup { 4, 4174 }`
  before the Haskell probe executes (`15-*`).
- Five repair parcels are committed but unmerged. Their reviews are split:
  Batch A merged, root E deferred, Batch B and C blocked, custody request
  changes, reload blocked.
- No frozen bundle was qualified.

## Decisions waiting for you

1. Root E: merge the narrow-closure selection fix, or build the differential
   against current first-publication behavior first? Deferring leaves Batch A
   case #11 red.
2. Batch B case 2: produce a production-path repro
   (`merge_compiler_inputs_inner` → `retain_merged_selection`) or correct the
   fixture; do not call it a product defect before that.
3. Batch C: the typed `CertificationError` cause is cheaper than reported.
   Decide whether to add the variant now or keep the string and strengthen the
   oracles first.
4. Custody b2: which layer owns multi-group, one-carrier availability? The
   review places it in `declaration_context.rs::normalize_issued_contributions`.
5. A1c: it is red at the join tip (`15-*`), so the question is no longer
   whether the join fixes it. Decide whether the missing `Tidepool.Effects.Core`
   is the Batch A #8 candidate-issuer gap, or whether the published example
   needs its own fixture dependency declaration. Do not reuse the scratch-tree
   `InputNoCompatibleCandidate` trace: that tree is a different base.

## Working rules that already apply

- Never edit the dirty interactive clone `/srv/swarm/checkouts/tidepool`.
- Builds and tests only through
  `swarm-build bash -lc 'cd <checkout> && JUST_TEMPDIR=/tmp just ...'`.
  Do not pass `--local-only` or `-c remote.enabled=false` to `just`.
- A compile is not a pass. Record source OID, exact command,
  selected/executed/passed counts, exit status and log path.
- Prefer deleting obsolete surfaces over adapters; backward compatibility is
  not a goal inside this campaign.
- When a parcel is merged, rerun its counted controls on the joined tip and
  update `OPEN-PARCELS.md` rather than leaving the claim in a thread.

## Suggested first tranche

1. Diagnose A1c's missing `Tidepool.Effects.Core` at the join tip; it is the
   only model-facing documentation test known to be red, and its Haskell probe
   has never been observed to execute.
2. Rerun the 44-name trunk-red ledger at the join tip and reconcile it with the
   32 failures in `TOOLCHAIN-SWEEP.md`.
3. Land the Batch A residue that is still open (#9, #17, #18) and decide #7/#8.
4. Delete `BrowseBounded` and `ViewIdentity`/`view_identity` with the next
   inspection cleanup, per the coherence audit.
5. Freeze one bundle and run one ordinary persona history through the packaged
   path before making any release statement.
