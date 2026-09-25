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

## Implementation record

- Shared prompts/fork skill: workspace `5864ae1` (published), Tidepool pin and
  template `8b92c6eea`, harness prompt/pin `7953684` and `5dd21b9`. Establish an
  executable shared boundary and failure invariant before dependent forks;
  distinguish delivered corrections from verified incorporation; choose ready
  obligations rather than compulsory recursive fan-out; retain reviewers through
  repairs and rebase when required by dependencies/conflicts. The harness Label
  example now matches the existing compiled shared recipe. Four changed shared
  files match the template byte-for-byte.
- Focused-test guard: harness `4f9199b` and `28798f2`, documented and used by
  project prompts at `5192e07`/`9bdcb3e`. No invocation wrapper existed; Reflex
  only classifies output. The new project script requires package/target/filter,
  refuses zero runnable selection, checks actual libtest execution counts and
  preserves failure exits. It supports ordinary libtest, not custom harnesses.
  Eight stub-runner regressions passed after binary-target support (`a105009`).
  Real Driver follow-up lifecycle tests passed 2/2 through that binary target.
  Real dynamic reply tests passed 2/2 in
  the isolated checkout; the integrated adapter-readiness target passed 1/1.
  Nonexistent and ignored-only filters were refused without running live tests.
- Reminder fix: `42e4947dc`, with timing-free regression adjustment `f52951121`.
  Request activation retains the original provider turn/time across duplicate
  publications; reminders require a later turn and matching open request. A
  pushed reminder cannot be retracted by this change; its wording identifies the
  request and explicitly says to ignore it after submission. Two facade and two
  actor observation tests passed in the isolated checkout. On integrated main,
  both facade regressions and the workspace-pin test passed (3/3, 488 skipped).
- Typed-tool design: `9f4b0128c`, [request-scoped-tools.md](request-scoped-tools.md).
  Existing installed handlers lack typed current-request access. The proposed
  primitive belongs to Replies, with structural site-type checks and exact scoped
  binding identity before borrowing input. No runtime primitive or review tool
  is implemented yet; the first next step is the focused type/borrow proof.

Verification logs live in /tmp/rsi-iteration3-*.log during this session. The first
review-recipe invocation selected a Nix-store extractor and was stopped without
claiming results; its replacement explicitly selects the matched local frontend
and worker built by `just exomonad-build`. The isolated reminder test initially
could not compile because required pinned submodules were absent; after initializing
them its focused checks passed. Future worktree setup should check required
submodule availability before spending a compile attempt.

The matched local reviewProvenance recipe completed: 6/6 assertions passed,
including exact-scope repair and retention of assigned Task decisions/gates.
It launched no native workers or providers. The direct focused invocation took
over 20 minutes with repeated compiler-worker startup; investigate a focused
entry through the existing shared-daemon recipe runner rather than duplicating
its process owner. No speedup is claimed from this observation.

Wave-11 preparation: the user explicitly requests a Sol root with multiple Luna
subtrees. Harness docs/rsi-iteration-3.md and NEXT.md now prescribe at least two
Luna component owners each delegating two meaningful Luna obligations, targeting
three domains: durable Store recovery, Engine boundary recovery, and Driver/CLI
restart. Root retains cross-component contracts and crash acceptance. This is a
deliberate topology experiment, not a standing every-leaf-must-fork rule. Existing
tree CLI refuses any prior root; supported restart admission is a concrete product
gap, while ambiguous interrupted states must fail closed rather than replay
arbitrary work. Power-loss durability and exactly-once remote attempts are not
claimed. Wave-10 history is archived in harness docs/wave10-handoff.md.

After its completed interview and handoff, wave 10 was stopped through the CLI
(exit 0); its tmux session no longer exists.

## Wave 11 launch — 2026-09-25 20:09 UTC

Launched on the user's instruction through `just exomonad-init`, with the
matched local tools, Sol Medium root, and one compiler worker. Build and launch
preflight passed. Root received the NEXT.md brief and began reading the assignment
and resolving the committed source baseline; nested Luna delegation is assigned,
not yet observed at launch.

- Run: `cbc4c903-1655-4b3d-a48a-c8eb3712bd77`; tmux: `wave11`.
- Tidepool binary source: `ec1b006686fd71227b393acc93df13fbfa586661`.
- Harness baseline: `3788fd25ff7b081ac123b66aadf1dab25c0f021a`.
- Workspace pin: `5864ae1eada2410fdd8e2e371b4ce14d556ba356`.
- Root thread: `01a0da2f-4342-7af2-b48b-fc4dd21661ff`; pane `%511`.
- Host evidence: harness `.exomonad/logs/cbc4c903-1655-4b3d-a48a-c8eb3712bd77.jsonl`.
- Launch command output: `/tmp/rsi-wave11-launch.log`.

The terminal's first Enter left the brief in the composer; a separate Enter
submitted it. Confirmed execution by the root's first shell call reading NEXT.md
and reporting the exact harness HEAD, rather than treating terminal input as
proof of delivery.
