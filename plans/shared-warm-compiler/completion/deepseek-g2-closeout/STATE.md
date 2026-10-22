# Campaign state after joined review

Updated 2026-10-21. This is the current ledger, not the original dispatch list.
Implementation reviewed: **c910d1aae0e64c76449bef8170b53400133cbd23**.
The new review/plan commit changes documentation only.

## Source, publication and preserved work

| Item | State |
|---|---|
| Campaign checkout | /srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020 |
| Campaign branch | g2-closeout/deepseek-20261021 |
| Reviewed tree | Clean at c910d1aae0 before the new documentation edits |
| Prior staff plan | 1d239474e50368a97f038c2f3d75c721626ff1d4 |
| Structural prerequisite | 402528b8322615ab635adb8fca1cfc719a684523, integrated by 0cfb964739 |
| Real remote | github — https://github.com/tidepool-heavy-industries/tidepool.git |
| Local replication | origin — /srv/swarm/checkouts/tidepool-shared-warm-plan-20261019 |
| Interactive checkout | /srv/swarm/checkouts/tidepool is unrelated/dirty; do not switch or edit it |
| G2 / frozen bundle / fresh Persona | NOT qualified by this review |

H's checkout /srv/swarm/checkouts/tidepool-deepseek-g2-h-20261021 retains dirty,
unqualified GhcPipeline.hs instrumentation (two recorded GHC errors) and a
completion-custody hunk. Inspect/preserve before assigning that checkout.
The committed H role closure is separately integrated; dirty WIP is not approval.

D's earlier three-file WIP is preserved as
/srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/d-wip-preservation/575fc4e485-three-files.patch
(SHA-256 ebeaec93cc420142092bbc572d5cdcadf78f16a4e2a5d083136c45eaf9508a41)
and diagnostic commit ecd8e6bbd3. Paths were GhcPipeline.hs, Timing.hs and
runtime/session/activation_input_tests/historical_original_recovery.rs.
Keep the logging revert/reapply history, including 0ea5b8e2a9 and d35fd2ac43.
Do not stash/reset/discard other workers' source.

## Integrated parcels and present review disposition

The 14 named component tips below, including C1/C2, were checked with
git merge-base --is-ancestor against c910d1 in this review; all are ancestors.
The explicitly preserved dirty H work is not part of that result. Verify ancestry
again when moving the campaign. Integrated does not mean accepted behavior.

| Area | Tip / merge evidence | Current disposition and next owner |
|---|---|---|
| P producer/consumer | 5fd3580d78; merges 2c17120615 and a7be854638 | Integrated, changes requested: P-ITEM/HOME/REUSE/SCOPE/HELD |
| P2 earlier consumer | 83ac54b3aa | Integrated lineage; old HOME-slot status is superseded by joined review |
| P3 oracle | 527e53cd04, c91e981743 | Integrated; passing smoke claims narrower than names; P3 repairs oracles |
| W segment witness | d2b6899f71 | Integrated; source review supports segment repair; W must finish ordinary route and real histories |
| N publication | 9f77dd17ab; merge 380d2766aa | Integrated, changes requested: runtime selection, retained output, started progress |
| H roles/history | 45c2c165a9, handoff b309f8bdb5; merge 74897f4eb3 | Integrated role repair rejected; preserve history oracle and replace owner census |
| Q1 logging | 827d4f9fc4, 00beb4c093; merge f900654416 | Bounds/async behavior supported; Q repairs Rust JSON decoder |
| Q2 census | 769ef0ed40 | Integrated; source review supports removal of full-payload comparison |
| Q3 trace | 393d6e5d8c | Integrated; source review supports one enabled-request aggregation |
| Q4 Original | 9809de64c4 | Integrated; obsolete facade authority fallback removed |
| C1 schema | 6dd7eadfbd | Retain evidence; no new production redesign admitted |
| C2 demand | 21d5403ec8 | Retain hypotheses; not a G2 dependency |
| Resource audit | 0604fed601 | Integrated evidence; remeasure before new jobs |
| Earlier R and V | Reports/preparation retained in this directory and evidence | Previous approval is not joined c910d1 acceptance; next roles defined in README |

This review used four read-only Sol reviewers. All completed; no product repair
worker, build or deployment was launched by this review. External DeepSeek
worker liveness is **unobserved**. The next supervisor must record real launch,
acknowledgement, parent, exact base, owned paths and terminal events.

## Execution evidence that remains valid, and its limits

- Joined six-case ordinary package cohort passed before c910d1. It does not
  exercise World segment-item roots; several adversarial assertions are weak.
- Three-target compile gate passed: build
  5a0fef0c-3a66-4a46-924a-e03b500775b7. Compilation is not history execution.
- Fresh-HOME focused control passed 1/1: build
  7f286f4c-4f85-4a26-b0b9-ba51b5940773.
- Merged Q1 structuredLogging passed 1/1: build
  800ea223-b196-4565-b952-d225e6d01a23. Recorded bound: 32 fields,
  814604 bytes; this does not qualify Rust malformed-JSON routing.
- Historical reader h-run5 failed 0/1 at
  "target package slot lacks package links", build
  ccb8eda5-ed80-4dfd-b7dd-a88a230ea4fe. Retained under
  g2-closeout-deepseek-20261021/p1-producer-ordinal-fix/h-run5/.
  Later fallback experiments were reverted.
- Discovery/all four notebook cases, cold/eviction/producer-removal histories
  and matched Persona qualification have no passing joined acceptance here.
- The new review ran no product tests. It independently inspected retained CBOR
  and source; its two finite counterexample models are not runtime executions.

Earlier closed-acquisition, HOME-census, affine-initialization and hot-query
results remain component evidence, not proof of current joined G2 behavior.

## Current blockers

1. Carry exact package roots through checked-item publication; repair projection
   scoping, HOME ownership and repeated prior completion together.
2. Remove historical implementation choice from owner inventory traversal.
3. Preserve latest runtime selection and compiler World atomically; carry actual
   host-write and started/completed evidence.
4. Complete ordinary witness reuse and distinguishing production tests.
5. Replace Q1's ad hoc decoder, resolve frozen-fixture ownership, then execute
   joined G2 gates. See [V](V-qualification.md) for delivery sequencing.

Do not treat later failure frontiers as passes or assume these are the last
possible defects. The first two small joined gates in README reduce uncertainty
before another full history/bundle run.

## Evidence index

Current controlling review: [REVIEW-c910d1](REVIEW-c910d1.md).
Detailed reports and artifact inspection:
/srv/swarm/evidence/inanna/g2-peer-review-c910d1-20261021/.

Prior evidence remains:
/srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/,
including PEER-PLUS-REVIEW-20261021.md, the four earlier boundary reviews,
package-witness-path-evidence.json, compare-package-products.py,
peer-review-package-byte-comparison.json, P/W/N/H/Q receipts and resource records.

The prior CBOR comparison corrected field 15 = type graph, 14 = bindings,
13 = frames. Do not revive the obsolete complete-package recommendation based
on that mistaken field interpretation.

## Resources

The last handoff recorded approximately 120 GiB scratch free, 102 GiB available
RAM and 35 GiB in build.slice after a supported 42 GiB retirement. These are
historical observations, **not current admission**. Preserve 100 GiB unreserved
scratch including expected output growth and follow current RAM/lease limits.
Use measured independent lanes; do not assume either unlimited cold builds or
a blanket single lane.

S keeps a fresh dispatch/merge ledger here as work starts, without overwriting
the reviewed-source/evidence boundary above.
