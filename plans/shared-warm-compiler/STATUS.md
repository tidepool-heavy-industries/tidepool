# Campaign status and restart entry point

State: **implementation active; G0 bootstrap and first area waves launched**.

Campaign owner: `/root/warm_compiler_campaign`, `gpt-6.1-sol` high.
Parent coordinator: `/root` (performance fork). External pre-fork delivery/release
owner is unconfirmed; coordinate that owner before release/main/live-host handback.
Campaign may publish its own branch only.
Campaign checkout: `/srv/swarm/checkouts/tidepool-shared-warm-campaign-20261019`.
Campaign branch: `campaign/shared-warm-compiler`.
Bootstrap source and contract revision: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.

Public `github` remote refreshed deliberately at kickoff (2026-10-19):
release `545d9950e5c3c3940f5b6bf84dfc5d16d6b1a89d` is ancestor of imported peer
`70a92c65a02a591f76590daf4e5bfa3e6fcbf723`, which is parent of plan `5b74c2f1fe`.
Created the campaign from release, fast-forwarded the required peer prerequisite,
then carried the reviewed plan. The peer delta is 127 files, 6774 insertions and
1520 deletions: pre-existing implementation, protocol/projection/fixture and
correctness work, not campaign output or a documentation-only prerequisite.
Optional-root repair `d24b5b20b04b70c61416d5bf68858916b7b54848` is contained.
No parent active checkout, release branch, live Persona host or compiler changed.

## Actual capacity and ownership

Parent granted **14 active agents including this supervisor**. Five area
supervisors plus eight leaf grants fill that ceiling. Maximum two direct live
children per area; deeper descendants need explicitly reassigned grants.
All explicit model overrides use fresh context. Dormant earlier researchers are
not reactivated. The table records actual returned task identities; area handoffs
record their leaf candidates, grants, settlement and review/fold receipts.

| Actual task under `/root/warm_compiler_campaign` | Model/effort | Leaf grants | Branch/check-out suffix | Current state |
| --- | --- | ---: | --- | --- |
| `host_inputs` | gpt-6.1-sol high | 2 | feature/shared-warm/host-inputs; tidepool-shared-warm-host-inputs-20261019 | ACK; selected_root high and selection_oracle high started |
| `owned_inputs` | gpt-6.1-sol high | 2 | feature/shared-warm/owned-inputs; tidepool-shared-warm-owned-inputs-20261019 | ACK; rows medium and source_lifetime high started |
| `warm_workers` | gpt-6.1-sol high | 1 | feature/shared-warm/warm-workers; tidepool-shared-warm-warm-workers-20261019 | ACK; protocol high started |
| `reload` | gpt-6.1-sol high | 2 | feature/shared-warm/reload; tidepool-shared-warm-reload-20261019 | ACK; generation_admission high and authority_review high started |
| `qualification` | gpt-6.1-sol high | 1 | feature/shared-warm/qualification; tidepool-shared-warm-qualification-20261019 | ACK; capture_sensitivity high started; baseline/resource route active |

Area source base is exact `5b74c2f1fe`; all simultaneous writers have isolated
real checkouts/indexes. Each owns its `plans/shared-warm-compiler/AREA/HANDOFF.md`.
Only campaign edits this STATUS and shared CONTRACTS. Every accepted leaf delta
requires independent exact base-to-candidate Sol review, bounded repair, area
fold and campaign fold with actual joined production/test consumer checks.
No implementation checkpoint has yet been accepted or folded. All eight actual
leaf identities are running under their named area: host_inputs/{selected_root,
selection_oracle}, owned_inputs/{rows,source_lifetime}, warm_workers/protocol,
reload/{generation_admission,authority_review}, qualification/capture_sensitivity.
All leaves use gpt-6.1-sol high except owned_inputs/rows at medium.

## Prerequisite execution evidence

Read mandatory compiler-failure HLS-REWRITE-HANDOFF.md and actual stronger Persona
report under `/srv/swarm/evidence/inanna/admission-analysis-review-20261018/overnight-70a-strong-persona/`:
`host-cases/e27eec134184eea75959071896799d075b55b040628a49a0e25165b9be2fab4e.json`.
One selected/executed/passed test, exit 0. Process and hosted cleanup confirmed;
compiler cleanup `not_observed`, observation complete; diagnostics incomplete.
This is focused prerequisite evidence, not campaign or release qualification.
Selection/custody/interface compatibility/exact issued edges/demand/current
runtime authority remain distinct. Required historical conflicts still refuse.

## Resources and checks

Campaign buck-out bind mount provisioned and verified via findmnt.
Kickoff observation: `/srv/build` 102 GiB free, preserve 100 GiB unreserved
including expected growth; memory 95 GiB available. Source/review work active;
no campaign compiler/build/test process admitted yet. Each area must measure
actual headroom and output growth before checks, use pinned admitted local-only
repository commands, and retain exact pending commands for deferred checks.
Installed `swarm-build --storage-status` failed (`exec: --: invalid option`),
so staged bounded-storage switches are not assumed accepted or available.
No bypass, arbitrary output deletion or shared daemon restart is authorized.
Qualification investigates the smallest supported admission route and available
exact baseline artifacts; compilation/execution remain unqualified until run.

## Next actions and finite frontier

Host: exact selected root/home census and independent oracle. Owned inputs:
immutable row borrowing and consumed-source lifetime. Warm workers: source-free
protocol and actual GHC lifetime analysis without speculative dependent APIs.
Reload: generation dependency issuance/callback bypass closure. Qualification:
independent baseline/oracle sensitivity and early joined production-path checks.
Pipeline successor waves after exact dependencies fold; do not leave accepted
work in leaves. Hard strategy gaps receive bounded Sol-high adversarial review,
close only their dependency cone, and drain every already-approved independent
successor/review/repair/fold/running-test before a MORNING decision handoff.
No native goal is created by this subtree. No deployment is authorized.

Explicit user wrap-up task, after all campaign subagent work settles:
qualification performs bounded top-down scratch-space analysis by checkout/output
owner, active versus idle use, independent required products/evidence, potential
reclaimable output and exact safe boundaries. **Analysis only**: no deletion,
unmount, GC or stopping others. Admission/headroom checks continue during work.

## Active shared decisions and findings

C2 exact census and C6 context provenance amendments are recorded in CONTRACTS;
affected owners must acknowledge the amendment commit before broad migration.
Owned baseline rows reuse existing value-sized RetainedArtifactRow ownership at
P6 and borrow that same issued row on next capture; no extra transport registry.
Package-universe source association is under bounded independent review: current
deployment manifest owns executable digests and a libdir path, while old EPS/lazy
objects survive requests. A newly read path/hash cannot authenticate their old
bytes. Existing owned deployment source proof versus clean per-operation universe
fallback is being evaluated; row/host/reload work continues independently.
Qualification found the stronger shared-workspace case missing from frozen
persona-startup roster and a capture observer that still labels failed-cycle hit
counts observed. Narrow production repairs and independent controls are active.
Installed supported metadata check `swarm-build just check-plan exomonad-host
tidepool-toolchain` passed (metadata only, no Buck/output growth), not compilation
or test execution. Observed free bytes108974096384 leave about1.49GiB above reserve;
no defensible cold compile growth bound exists yet. Exact pending compile:
`swarm-build just check exomonad-host tidepool-toolchain`.

## Active supervision

Explicit user directive propagated to all five supervisors: stay active through
approved implementation, running children/builds, reviews, repairs, folds and
ready successor waves. Wait on interruptible mailbox events for at most60s when
only events can advance work. Final does not schedule continuation. If a required
parent has ended, inspect status/capacity and use followup_task with concrete next
action under its existing grant. Current host/warm ACKs received; remaining ACKs
being reconciled. Host oracle reviewer has temporarily settled after exact leaf
source review; its grant remains reserved for repaired/cumulative review.

Qualification source-only baseline executed89 unittest cases and failed19
subtests:16 new observer-sensitivity failures and3 fixture/procedure defects
separately classified. Buck exit32, evidence at
`/srv/swarm/evidence/inanna/shared-warm-qualification-20261019/capture-sensitivity/before-repair.{json,log}`.
This is actual failure discrimination, not passing acceptance. Buck logical output
75,974,066B under measured100MiB lane; native checks remain disk-blocked.
Warm structural candidate2ea510f075ccc8b7a7f5620faac884e610666b61 independently
source-reviewed full5b74..candidate by campaign; redundant explicit HscEnv fields
removed with identical sole HPT fallback and missing-HMI behavior. No native compile
or memory-release claim, no qualified fold. Package default cold fallback held
pending actual source-owner option review and regression controls.
