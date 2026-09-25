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

### Launch repair verification

Tidepool `22133ae88` integrates the repair from isolated worktree
`codex/launch-cleanup` (`14042ebea`): collected host cleanup, durable-evidence
journal opening, and a nonmutating early binding-lock probe. Active wave 10 keeps
its frozen executable. Independent readers checked recovery, cleanup ordering
and the lock boundary. No active run was restarted for validation.

- `just test-target exomonad-worktree worktree 'test(binding_owner_probe)'`: 4/4
  passed, 119 skipped; missing/held/released locks, untouched rows, I/O refusal.
- `just test-lib tidepool 'test(stop_host_unit) | test(later_host_) |
  test(a_later_generation_) | test(recreate_on_a_session_)'`: 7/7 passed,
  482 skipped; mocked systemd states plus journal and root-mode tests.
- First attempted target `worktree_core` was not a Cargo test target; corrected
  to its actual owning target `worktree`. That attempt ran no tests.
- Rust formatting and diff checks passed. Real systemd crash/restart integration
  was not exercised; the running wave is not a test fixture.
- Retained logs: target/tidepool-test-runs/launch-cleanup/{worktree,facade}.log.

### First live review-basis observation

Wave 10 root admitted the Store exact review at 18:46:18Z with the five-argument
reviewCommit call, base f9ab1a9 and candidate 5b2741a. Reviewer accepted at
18:48:21Z using ReviewedCandidate (reviewBasis current), without constructing a
Task. Root integrated at 176a271 at 18:49:04Z, then the owning provenance test
executed 1/1 on the integrated head. This is one successful real opportunity for
the new API, not a claim that the whole wave or automation experiment succeeded.
The reviewer had a multiline-cell parse rejection before a successful one-line
retry, and explicitly left unexecuted race/cleanup gates open. Evidence: wave-10
root/reviewer rollouts, Git, and read-only observer report.

### Background debt audit

- Implemented in `d23e0193a`: predecessor recovery carries typed actor identity
  instead of deciding root safety through starts_with("1-"). Original directory
  labels remain available for notices; root-ID convention is unchanged. Four
  focused recovery tests passed (486 skipped), including numeric root label
  `01-2`. Formatting and diff checks passed. Retained log:
  target/tidepool-test-runs/launch-cleanup/typed-predecessor.log.
- A pre-root retry still receives generation-based lost-state/recovery wording.
  Determine startup vs recovery from evidence for status/notice accuracy; this
  is an observability follow-up, not permission to weaken recovery requirements.
- Session-level and run-level root binding paths share ambiguous names and
  scattered existence checks. Consider a typed observation and explicit names
  after tracing all consumers; no new path resolver or registry is warranted.

### Engine review observation

Read-only monitor observed a successful Repair submission at 18:51:57Z for exact
Engine candidate `2d12a4a`. Findings were state consumption before dynamic schema
validation and ignored string patterns. These remain reviewer findings pending
repair and integration checks. The reviewer reported two tests executing 1/1
each and excluded an earlier zero-match invocation. Four rejected Haskell reply
cells preceded the successful reply: parse errors and unavailable bindings after
failed cells. Retain this as model-facing friction for the post-wave interview.

### Reply rejection audit and next experiment

The detailed rollout audit supersedes the preliminary count above: Engine had
five rejected cells before its first successful Repair. At 18:51:10Z an extra
closing parenthesis failed parsing; at 18:51:20Z a multiline list binding failed
notebook input-unit parsing. Subsequent attempts referenced uncommitted `latest`
or resolved `findings` to an unrelated existing binding. The inline-list reply
succeeded at 18:51:57Z. Store's multiline list binding similarly failed at
18:48:03Z and its single-line retry succeeded at 18:48:21Z. These were not
ReviewDecision API type errors. Engine's later redundant replies failed because
`respond` was no longer in scope after successful submission; interview why it
attempted another reply. A later request was accepted at 18:58:27Z.

Evidence: native rollouts ending 01a0d9e4-22de-7b50-a96b-e30a5db5168c (Store)
and 01a0d9e6-f251-7e02-a97b-d84d7723c21b (Engine), dated 2026-09-25.

User proposes tools tailored to the requested value so bounded Luna reviewers
need not author Haskell. Investigate a typed review submission tool that captures
the current request/basis and accepts the judgment and evidence, using existing
tool schemas and reply authority. Preserve exact-candidate checks and ordinary
typed replies; do not create a second review protocol. This is a proposal, not
an implemented or tested tool. Assess request rebinding and single-submission
behavior before implementation.

Contract audit: the brief already required a shared scaffold before forks, but
the next scaffold should exercise the real message projection and strict tool
ingress. The evolving harness NEXT.md retains the superseded `typed_result`
signature alongside its correction; consolidate its current-contract section
after the active wave. This is an incorporation/documentation issue. Test prompts
already exclude zero matches; retain that policy and give exact target/filter
and expected count in the gate packet once named, rather than adding more rules.

Post-wave root interview must cover these incidents, correction incorporation,
review-tool ergonomics, evidence counts, and the final integrated behavior. Ask
whether reply success was clear before redundant submissions. Compare answers
with traces before selecting the next implementation changes.

### Post-wave proposal for root feedback

Ask after the integrated gate and stop handoff, without reopening the product
assignment. First ask for unprompted observations, then test these proposals:

1. Fix ordinary multiline notebook bindings at their owning parser/renderer
   boundary. Implementation is delegated; preserve valid Haskell semantics and
   demonstrate the actual rejected review-cell forms in regressions.
2. Add a typed review submission tool alongside Haskell. Capture the current
   ReviewRequest and basis, accept the judgment/evidence, and use the existing
   reply mechanism. Keep Haskell available for pagination, exploration and
   coordination. Ask which other repeated operations merit tools only after
   observing this first addition. Do not remove the notebook in this iteration.
3. Keep one current shared API in the brief. Before the next split, execute one
   representative producer/consumer path through real serialization and strict
   input schema boundaries. Ask which concrete example would have exposed this
   wave's contract amendments before independent implementation.
4. Put exact test target, filter and expected matched count in the release gate
   packet. Existing prompts already demand honest counts. Ask whether zero-match
   refusals belong in the project's existing runner, based on actual rerun cost;
   avoid adding another check subsystem solely for this wave.

Root interview: what cost the most avoidable work; which proposals would have
helped; whether anything here duplicates an existing capability; how request
state/notices contributed to redundant replies; and which single experiment
would most improve the next wave. Separate reported friction from verified
failure paths and preserve unresolved contradictions in the evidence.
