# Next stage: reviewer brief

This is a seed prompt for a peer++ reviewer taking over the shared-warm
campaign. Replace the branch and OID if the join has advanced.

## Read in this order

1. `plans/shared-warm-compiler/README.md` and `STATUS.md` for the campaign
   boundary.
2. `plans/shared-warm-compiler/mid-plan-notes/MILESTONE.md` for what actually
   landed and what was executed.
3. `plans/shared-warm-compiler/mid-plan-notes/OPEN-PARCELS.md` for unmerged
   work and known product defects.
4. `plans/compiler-query-effects.md` for the five-verb decision and the
   acceptance matrix.
5. The adversarial reviews under
   `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reviews/`.

## State you are inheriting

- Join branch `integration/compiler-query-warm-final` at `a4aa30a0a2`.
- It builds; five counted cases pass (query dispatch, persona successor,
  front-door fixture) plus an analysis-time schema check.
- Five repair/product parcels are committed but unmerged; one product defect
  (Batch B case 2) and one red documentation test (A1c) are known.
- No frozen bundle was qualified. Do not describe the tree as release-ready.

## Decisions waiting for you

1. Root E: merge the narrow-closure selection fix, or keep it open while
   building a differential against the current first-publication behavior?
2. Batch B case 2 / custody b2: which layer owns multi-group, one-carrier
   availability, and is a typed refusal needed before the fix?
3. A1c: is the retained `Effects.Core` interface a stale fixture input, an
   extractor packaging bug, or an admission defect?
4. Second-cell case 3: migrate the stale fixture module to the transaction
   shape, or delete the module if it no longer describes a real workflow?

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

1. Land the Batch A/B/C test repairs after their adversarial reviews, rerunning
   the 44-red ledger on the joined tip and recording which reds remain product.
2. Resolve A1c root cause; it is the only model-facing documentation test known
   to be red.
3. Decide root E with a differential measurement, not by inspection.
4. Freeze one bundle and run one ordinary persona history through the packaged
   path before making any release statement.
