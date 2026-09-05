# Request progress channel

Implementation contract for the approved reliability follow-up. Source changes
stay in the coordinator checkout; live model canaries remain deferred.

Progress is a request-scoped single-producer, multiple-observer latest-value
channel, not an MPSC queue. Reply authority authorizes the producer; sharing
an observation handle does not confer publication authority. Each accepted
publication increments a request-local revision under the request registry
lock. Independent observers keep their own cursors. There is no shared read
position and reading does not consume a value.

`requestWithProgress @Progress @Result` preserves the ordinary final response
and adds a typed progress handle. The mounted `reportProgress` accepts the
request's monomorphic progress type. Ordinary requests remain unchanged.
Progressive unfold branches follow the same contract.

The registry retains only the latest value using existing managed heap custody.
Session-defined ADTs and closures require neither serialization nor `Show`.
Replacing the latest value releases the registry's old root. Already observed
bindings and ready watches retain their own custody and remain valid through GC.

`awaitProgressAfter handle cursor` observes a revision strictly greater than
the supplied cursor. Registration and readiness inspection are atomic in the
existing request/watch owner. A one-shot watch captures the latest qualifying
snapshot when that dependency first qualifies, even while other dependencies
remain pending, and remains stable on repeated polling. Intermediate
publications may coalesce. Consumers explicitly rearm with the captured revision;
unwatched publications never wake the coordinator.

Settlement, abandonment, acknowledged cancellation and retirement close the
channel and reject later publication. A waiting watch receives a typed terminal
event when no qualifying snapshot exists. A ready snapshot is not overwritten
by closure. Terminal handling releases registry references without invalidating
independently retained values. No unbounded event history is kept.

Acceptance checks cover independent cursors, coalescing, publication versus
registration races, stable ready snapshots, wrong-owner rejection, every terminal
path, and managed values surviving replacement and garbage collection across
workbench calls. Extend the existing request registry, Await/Watch evaluator,
and managed custody owner rather than introducing a parallel subscription system.

Other approved work remains provider observation/health and deduplicated
supervisor failure notices, backend provenance, typed retirement disposition,
and corresponding prompt/status documentation and deterministic checks.

## Acceptance evidence

- Provider reader: nine focused tests pass, including failed completion with
  partial usage, failure followed by recovery between polls, own-thread context,
  replay identity, and delayed usage records.
- Runtime observation: eight tests pass, including stale-read preservation and
  retirement requiring positive successful idle evidence with no requests.
- Durable notices: host event round-trip and publication/reopen tests pass;
  six inbox tests cover deduplication, compaction, and numeric-cursor migration.
  Host publication stops at the first write failure to preserve watermark order.
- Progress: registry closure tests pass for reply, abandonment, acknowledged
  cancellation, retirement and wrong-owner observation. The hosted ADT/closure
  test passes through producer retirement, independent cursors, and a
  progress-plus-response watch preserving its first captured closure.
  Publication-before-registration and registration-before-publication are both
  covered there; both operations hold the same registry lock through readiness
  evaluation. The threaded registration/settlement race also passes.
  The production publication guard rejects unauthorized actors, wrong
  incarnations, unpresented requests and abandoned requests.
- Public Haskell surface including progressive unfold compiles and passes.
  Framed-handle GC test passes with forced nursery collection and root cleanup.
- Generated protocol check passes. Fixture regeneration changes only the source
  fingerprint; all 217 fixture semantic tests pass.
- Extractor wrapper: all 12 tests pass, including persistent daemon ownership.

The complete request registry suite passes all 26 tests. The hosted cleanup
regression passes: a stale provider observation blocks execution before metadata
removal, and restoring confirmed idle evidence allows the original plan to run.
Unavailable combined watches release their captured roots; ready snapshots keep
custody until explicit watch forgetting. Publication replacement and terminal
request cleanup release registry roots through the existing custody owner.

The Shoal binary builds and its help command succeeds. Final formatting and
candidate review precede the local commit. Live model canaries and restarting
the current host remain deferred; the host retains its loaded snapshot.
