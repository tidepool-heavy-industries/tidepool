# RSI iteration 3 — decisions after wave 10

## Evidence

Wave 10 integrated at harness `a1c8cd9`; final focused gate matched/passed
13 tests, plus formatting and diff checks. Handoff `76eba2b`; post-wave root
interview `6005c7f` in harness docs/interviews.md and docs/exomonad-friction.md.
The live adapter, item-2 retry and item-13 trace holds remain. Clean Store reopen
was exercised; process-crash recovery was not established.

Root ranked late atomic publication repair, changing producer/consumer contracts,
then manual source/check reconciliation as the largest avoidable costs. Its
highest-priority recommendation was an executable boundary contract including a
failure case before independent implementation. External trace audit adds a
concrete stale reminder defect that the root could only infer from interviews.

## Recommended preparation changes

1. **Fix stale automatic request reminders.**
   `bridge/facade/src/actor_host.rs::remind_turn_ended_without_respond` combines
   the currently open request with an older provider idle observation. Store
   request 13 activated at 19:04:18.344Z; reminder pushed 19:04:19.834Z using an
   older idle period; reply committed 19:04:38.170Z; reminder presented
   19:04:40.091Z. Engine review request 11 shows the same sequence. Correlate the
   provider turn with the request activation and ensure queued reminders cannot
   demand a second reply after settlement. Investigate the existing delivery
   owner rather than adding another queue. Tests: new request/old idle period,
   genuine unanswered completed turn, settlement before reminder delivery, and
   one reminder per eligible period. No evidence of a dropped reply.
2. **Repair the project review example.**
   Harness override still uses a string annotated Label; the shared/template
   prompt uses the compiled `[label|repair-candidate|]` form. Driver review
   actually rejected the old form at 19:05:16Z. Root did not observe that detail;
   retain the trace finding rather than treating its lack of observation as
   contrary evidence. Compile-check the edited snippet through the existing
   recipe, without adding another prompt-check subsystem.
3. **Enforce honest focused-test selection at execution.**
   Put package, target, filter and expected count in gate packets. Investigate
   extending the existing project runner to refuse zero matches and retain
   concise counts/output references. No new scheduler; an exit-zero command
   without matched tests is not a passed gate. Multiple participants reported
   zero-match false starts even though prompts already prohibited counting them.
4. **Add typed review tools incrementally, retaining Haskell.**
   Start with acceptance submission; keep notebook exploration, pagination and
   repair available. Installed Project.Tools handlers currently compile outside
   the active request workbench, so capturing initial sessionInput is unsound
   when reviewAgain reuses a reviewer. First establish the smallest generic
   current-request capability for authored typed tools; do not hardcode review
   in the host or create a second reply registry. The handler derives basis and
   candidate from the current request and submits through existing Replies.
   Acceptance cases: AssignedTask/ExactScope, revised request, stale request or
   candidate, different checkout HEAD, duplicate invocation, transport retry,
   and no active review. This needs a design check before implementation; a
   workspace-only helper is not equivalent to a safe separate submission tool.

The valid multiline placement fix is already integrated (`dffd88db3`). Automatic
indentation repair remains deferred in rsi-iteration-2.md.

## Next wave proposal: offline crash/restart lifecycle recovery

Proposed scope, not a launch authorization: extend the real Driver lifecycle
through process loss around atomic completion and notification. After restart
over a file Store, discover durable parent answers and pending child follow-ups
exactly once, preserving typed results and provenance. Include the boundary after
atomic head/answer commit but before the wake hint, plus rollback before commit.
Define exactly what is restarted; clean close/reopen cannot substitute for crash
evidence. Keep live inference, contract versioning and adapter implementation
outside this experiment.

Before forks, root lands one canonical current API brief and a compiling fixture
through the real service/Store/Engine/Driver boundaries, naming durable invariants
and an explicit failure/restart barrier. The relevant case may initially be
expected-red; record that honestly. Do not require completing the implementation
before delegation. Reuse the accepted contract and existing checks, and assign
owners around actual source boundaries rather than a mandatory child count.

Use exact dependency OIDs, consumer paths and incorporation evidence in updates.
Keep source-aware routing as an authored coordination experiment first; this wave
does not yet justify a new registry. Measure contract amendments, stale candidate
or checkout incidents, zero-match runs, reply retries, and defects caught before
versus during review. Preserve independent review: wave 10 caught consequential
bugs despite green happy-path tests.
