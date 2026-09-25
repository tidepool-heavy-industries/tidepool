# RSI iteration 2 / wave 10

Launch authorized by Inanna on 2026-09-25 with ordinary event-driven review.
The committed harness NEXT.md and docs/rsi-iteration-2.md at 076684b are the
execution brief. Product work remains owned by the harness wave; the external
supervisor prepares Exomonad, observes, and evaluates.

## What wave 9 taught us

Wave 9 merged a single new test at harness `4230179`, with handoff `9c6e75b`.
The actual root task began at 17:56:58Z, integrated checks finished by 18:04:01Z,
and the handoff commit was at 18:05:17Z on 2026-09-25. This excludes preparation.
The integrated first-request and adapter-readiness targets each executed 1/1.

- Typed review base and exact candidate were preserved in one real review.
- Event waiting worked on the observed opportunities; retain it unchanged.
- The product assignment only needed coverage of existing behavior. It did not
  exercise parallel implementation, contract evolution, or repair composition.
- The implementation request wrongly required Delivery while root owned review
  and integration. Progress preserved the candidate; Blocked settled the malformed
  assignment honestly. This was recoverable coordination work, not product repair.
- Exact-commit acceptance requires the reviewer to construct a Task. That is an
  API mismatch, not merely a missing prompt reminder.
- Automatic review was preflight-blocked by generated wrapper type names; no
  live automatic-coordination success can be inferred.
- Manual head advancement in the test limits its claim to persistence. The next
  release gate should use the real driver for lifecycle and parent publication.

Choose the smallest wave that teaches enough to meaningfully change the next
one; size and duration are not success criteria. For wave 10, deliberately expand
beyond wave 9: implement and integrate the complete lifecycle below, with shared
contracts and production consumers. Faster successful execution is welcome.

## Preparation improvements

The review-basis fix is the intervention shipped for wave 10. The launch packet
explicitly assigns Outcome Candidate to bounded implementation children. The
wrapper repair and automatic composition below remain follow-ups, not launch
prerequisites. The user accepted ordinary event-driven review for this wave.

1. Make project implementation admission preserve its result stage. Reuse the
   existing `implement` owner in Project.Work; inspect its fixed model selection
   before adapting it to the Sol execution policy. Keep generic runtime forks
   available for arbitrary authored contracts. Compile the published example.
2. Implemented: ReviewBasis preserves either AssignedTask or ExactScope; one
   ReviewRequest carries it through admission and acceptance. Exact reviews
   return repair findings to the requester; no Task is fabricated. Workspace
   `7307478`, Tidepool `4e4a62396`, harness `705eaee`. The focused provenance
   recipe passed six assertions in both Tidepool and the pinned harness workspace.
3. Repair generated wrapper type preservation at the compiler/workbench boundary.
   No fixed qualifier whitelist or silent removal of semantically needed pins.
   Acceptance: the actual automaticReview recipe executes its assertions, plus
   focused regression coverage for the rejected inferred types.
4. Compile one end-to-end authored candidate-to-review composition. Test a real
   repair path offline; never introduce a deliberate defect into wave product
   work just to force a live repair opportunity. Root retains integration judgment.

## Proposed product milestone

**An agent can finish honestly while new work arrives, and the harness delivers
both the answer's input provenance and the next task without losing either.**

This combines amendment 4 with the minimum durable envelope identity needed to
make it observable. Full acknowledgement/incorporation protocols, contract
versioning, message replacement, adapter implementation and live inference are
outside this proposal.

Existing mechanisms to extend: Store envelope identity/delivery records,
StoreAgentToolService.followup_task, Engine request-boundary admission, and the
demo Driver's completion/head-CAS/reactivation path. Driver already rescans unread
work and advances the head; do not build a second scheduler. Its production
factory currently uses Engine.run; establish how typed finalization reaches
parent publication before delegating that boundary.

Root first decides the shared contract: envelope references; request-visible
input provenance; the precise snapshot at which unseen work is reported; typed
completion and parent reply; ownership of persistence and continuation. A final
answer is not evidence of incorporating input that its request never contained.

Then delegate meaningful obligations with disjoint owned paths where practical:

1. Store/service: stable returned envelope references and durable provenance
   query/transaction boundaries, extending existing storage.
2. Engine/completion: carry the final request's observed inputs into typed
   completion; preserve answers when newer follow-ups are pending.
3. Driver integration (root-owned or one explicit owner): publish the answer with
   provenance, advance the real head, and continue pending work exactly once.
4. Independent review of the integrated behavioral contract and exact candidates.

The acceptance owner supplies fixtures early; do not assign overlapping Engine
or Store edits to several leaves merely to achieve parallelism.

## Scope and learning budget

The required outcome includes sender-visible envelope identity, structured typed
answer publication with input provenance, and continuation through the existing
driver. A lower-level passing test alone does not complete the wave. Establish
the common contract first, then use two or three implementation owners where
source ownership permits. Root owns the combined gate and integration.

This gives us real opportunities to observe baseline incorporation, independent
implementation against a shared contract, candidate review, and integrated race
handling. Retain event waiting and test the unified review request on real
candidates. Automatic review is an additional experiment only if its prerequisite
works; its repair must not become an unbounded launch dependency.

## Release scenarios

Use offline ReplayTransport barriers and the actual service, Engine and driver:

- Follow-up arrives while the child's final request is held. Parent receives the
  valid answer with that follow-up identified as unseen; no refusal/retry loop.
- The child continues and sees that exact follow-up once in its next request,
  produces a second typed answer, and does not start duplicate concurrent runs.
- Follow-up already presented is classified as seen. An arrival after the defined
  completion snapshot is still discovered; it is not retroactively claimed seen.
- Driver advances head without test-side `advance_agent_head`; file Store reopen
  preserves reply, request and envelope linkage. Do not claim crash recovery from
  a clean reopen; any crash guarantee needs a separate explicit fault test.
- Existing active-cell and first-request gates continue to pass. Cancellation and
  failure checks preserve existing authority and cleanup contracts.

## Orchestration experiment and assessment

Use a shared compiled baseline, two or three implementation obligations where
the source boundaries support them, and a retained independent reviewer. Route
candidate-to-review mechanically when preflight is green; route ambiguity,
contract changes and integration decisions to root. If preflight remains blocked,
state the loss of that experiment explicitly before launch.

Measure useful accepted behaviors, candidate-to-review latency, manual relay
turns, rejected cells, result-stage mismatches, idle polling, stale source repairs,
and integrated failure paths. Report preparation separately from run execution.
Interview on what required judgment and what repeated work could become authored
code. A shorter-than-expected successful wave is not a failure.

## Operator launch friction

The supervisor is also an Exomonad user; these are RSI findings.

- Initial attempt b48bf7e2-a7c5-4eef-875a-19b3f5eddc6b passed preflight but
  failed at the workspace binding lock held by completed wave 9. Explicitly
  stopping wave 9 through the CLI released it. Consider detecting the actual
  lock owner before allocating a new run, with actionable retained-run status.
- That attempt auto-retried before ever binding a root; after the lock cleared,
  it failed with `recovered host has no actor lifecycle journal`. Investigate
  consistency between the no-root fresh-start decision and lifecycle recovery.
- `exomonad stop` then failed because its systemd unit was already gone, leaving
  the compiler tmux window. Plain init refused the existing session. The documented
  `init --recreate` path failed on the same absent-unit condition. After verifying
  the unit was inactive/not-found and only the compiler pane remained, the
  supervisor sent SIGTERM to that exact compiler PID; tmux closed and a fresh
  init was started. Cleanup should handle
  an absent host unit while still deliberately releasing remaining run resources.
- No native model task started in that failed attempt. This is launch friction,
  not a harness-product failure. Logs: /tmp/rsi-wave10-launch.log,
  /tmp/rsi-wave10-launch-retry.log, /tmp/rsi-wave10-recreate.log; durable initial
  host log in harness .exomonad/logs/b48bf7e2-a7c5-4eef-875a-19b3f5eddc6b.log.
