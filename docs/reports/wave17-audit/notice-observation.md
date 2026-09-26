# Wave17 queued settlement notices: observation gap

2026-09-26. Read-only source: run `ae20a047-cb39-41e1-8621-efd3035fd719`, checkout `/home/inanna/dev/exomonad-harness-runs/wave17`, root transcript `rollout-2026-09-26T11-23-19-01a0def5-1692-79f2-a2aa-a11aa7b92d58.jsonl`. Transcript references below are its ordinals and call IDs. This is a deferred runtime change, not approval to suppress notices before the next launch.

## Observed sequence

The root read the original browser failure from retained job `9928d6c5` without rerunning it (`384/386`, `call_xN93uzAQDXY4k25jdcVlsVgy`). It then repaired and integrated the test and read the clean browser and four component results at `6c28a79` (`798/800`, `805/807`, `901/903`, `908/910`, `936/938`). After its completed handoff, the model received earlier ordinary settlement messages: request 10's already recovered browser failure at ordinal 1048, request 11's superseded `9358cf2` documentation candidate at 1059, the old `7c7f8bb` component checks at 1264–1326, and the clean `6c28a79` gate messages at 1381–1447. These were distinct queued events delivered late, not evidence that one event was sent twice. The model spent turns classifying their source and prior handling.

The run did not call `Project.Routing.followWork`, `notifyWork`, or `incorporatedWork`. `Project.Routing.handledWork` stores only `(source name, Candidate)` to remove exact progress evidence from `outstandingEvidence`; it has no request or inbox sequence identity. Candidate equality cannot establish whether a later failure, question, changed check report, or contradictory terminal receipt was already read. Suppressing `WorkFinished` from that state would also leave the wave17 path untouched.

## Owning seam

`bridge/facade/src/actor_host.rs` renders `TypedActorEvent::SettlementChanged` as the ordinary request or job notice (around lines 1390–1420). `deliver_pending_checked` (around 5840–6028) acknowledges an inbox batch after backend push. Before pushing, it can silently acknowledge a queued **watch** notice whose exact watch was forgotten or whose settled transition the owner already observed. `deliver_out_of_order_notices` applies the same watch rule behind a stuck tracked message. The durable inbox retains the original sequence and receipt; its existing acknowledgment fence should remain authoritative.

The predicate comes from `exomonad/actor/src/request.rs:1880–1919`: `observe_watch` records when the owner saw Ready or Unavailable, and `watch_observed_since` compares that observation to the notice's transition time. `observe_response` at about line 1420 returns Ready or Unavailable but records no corresponding owner observation. Thus the host has no authoritative predicate for an already read settlement. A source OID, rendered text, or root handoff does not supply one.

## Narrow candidate and gates

In the request registry, record an owner-authorized terminal `pollResponse` observation keyed by exact request ID, terminal transition, and observation time. Expose a predicate for the same request/transition observed at or after the queued settlement's occurrence. Apply it at both ordinary and out-of-order inbox delivery, mirroring the watch path. Keep the queued row and typed evidence until ordinary durable acknowledgment; do not parse its preview or infer supersession from commit ancestry.

Focused tests should prove: an unread terminal settlement is delivered; the **same** terminal request already read by its owner is acknowledged without another prompt; a different request, changed terminal transition, unresolved delivery, or new failure is still delivered; out-of-order delivery and restart preserve the inbox fence; command-job notices are suppressed only if the command's exact request has the same proved observation. The current `pollResponse` observation API and request lifecycle need inspection before implementation so a status view, a watch on another request, or a stale handle cannot accidentally authorize suppression.

Supersession is separate. Request 11's `9358cf2` and later `07d7c8c` represent different typed requests and evidence. Nothing in current request identity says the later one semantically replaces the former. An explicit owner-issued relation at request creation would be needed before suppressing that older notice. Until then, retain it and show its exact request/source identity.

No runtime code or tests changed in this audit. The observed cost is repeated model handling; no controlled time or token saving was measured.
