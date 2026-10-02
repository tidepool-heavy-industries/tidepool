# Retained runtime investigations

This file keeps only evidence still named by production source or a retained
failure. Earlier wave proposals and completed fixes are available in Git
history.

## Wave-3 evidence referenced by source

Run `8a782b2b` recorded 21 of 124 cell rejections involving `Text`. Fresh-context
children saw the qualified `T` alias and guessed `Text.pack`, although the type
was available unqualified. The MCP preamble now imports `Data.Text (Text)` at
the alias declaration, and its regression test checks that import. Source comments
retain this count as the rationale for making the alias explicit. This is a
completed correction, not an open implementation proposal.

The wave also exposed single-session contention for selected-context children;
that historical finding is superseded by current runtime session ownership and
acceptance contracts.

## 2026-09-28 — retained-review fixture stalls after candidate receipt

On recursive-delegation workspace definition
`88cc6e51c885840c6e9a3680540b80bc41b25b68bcdaca606436f6583cd301e3`,
`Project.RoutingChecks.automaticReview` receives the real candidate/submitted
HEAD, records no source refusal, and retains no reviewer collector. Waiting for
the retained reviewer's next activation times out. `Actor.pollExit` returning
Nothing excludes a terminal exit, not a paused handler. The runtime can preserve
a failed handler in `ResidentStanding::Paused` until intervention. Inspect the
current `R.lifecycle` snapshot and Paused detail before another retry.

Evidence: `target/tidepool-test-runs/20260928T072118Z-2621728-exomonad-check`
and `docs/reports/recursive-delegation/verification/retained-review-final.log`.
Request ordering is reserve → retain callback → submit; distinguish a paused
callback from failure to activate the target. The separate exact-source review
flow passed its source-mismatch, revised-review, publication, and bounded repair
checks. Keep this older fixture failure distinct; a candidate receipt is not
proof of review completion.

## 2026-09-28 — calls waiting behind a failed source mapper

A dynamic-attachment reproduction initially retained the wrong Haskell payload
field. The mapper failed with reusable `UnresolvedCallee` after session checkout;
a following synchronous `R.call` waited until timeout. Correcting the payload
fixed event delivery and the replacement/cleanup regression passed. This was
not checkout contention.

Pending-call failure visibility remains open: determine whether an actor paused
by a source-mapper failure should promptly refuse a queued call or expose a
recoverable retained wait. Do not silently change cancellation or actor recovery
semantics. Evidence: dynamic-sources worktree
`target/tidepool-test-runs/20260928T211101Z-3871481-battery`, nextest run
`7d709d28-931b-4316-aeef-cfc6e317a76b`, session `16632024630559531`, Watcher
`2@1`. The recorded batch fixes the invalid mapper, not this general failure
reporting path.

## 2026-09-28 — queued checkpoint backlog

At 22:16 UTC, root 1@1 of run `fc8dae5b-b02e-4884-959c-12cc060b1b8c` had durable
inbox checkpoint sequence 202 and accepted messages through 325. Operator
readiness notification 319 was retained but not presented. The root separately
checked ancestry of delayed component checkpoints against newer retained source.
This demonstrates a queue/backlog and redundant reconciliation; it does not by
itself identify transport loss or prove a scheduling fix. A console readiness
request was queued explicitly and observed being read.

Next observation: distinguish required obligations from superseded progress;
trace admission, presentation, and model turns before changing delivery. The
optional digest coordinator is a bounded trial, not evidence that an existing
FIFO backlog is solved. Measure concrete owner relays and rereads removed while
preserving the original evidence.
