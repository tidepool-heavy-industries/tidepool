# RSI iteration 1: programmable coordination

Authorized 2026-09-25. Supervisor owns preparation, observation and iteration;
the Exomonad root owns the harness assignment and checked integration. This is
early alpha development: substantial new capabilities are welcome when evidence
or a concrete orchestration experiment motivates them.

## Intended outcomes

1. Advance the harness through a useful, independently reviewed slice.
2. Move recurring coordination into inspectable typed programs.
3. Discover what orchestration patterns the existing primitives enable and where
   a new primitive would change what can be expressed.

Default product task: an offline file-Store gate for a follow-up queued before
the target's first model request, admitted once, followed by typed finalization
and a reopen query. Inspect existing coverage first. Queueing itself already
exists in `agent_runtime.rs::followup_task` and its arrival-order unit test.
No claim of a missing implementation follows from the proposed gate.

## Wave-8 reconciliation

| Evidence | Current disposition | Next action |
|---|---|---|
| Exact offline adapter-readiness gate merged at harness `703005f` | Completed; fake resident evaluator, no live adapter or crash recovery proof | Preserve its acceptance boundary |
| Root's long idle/polling interval; wave-8 transcript and interview | Harness `b2b8936` already tells root to end idle turns | Test actual exposure and behavior; inspect completion routes on silence |
| Incorrect review base and stale review checkout | `b2b8936` requires mechanically resolved OIDs and actual HEAD; `CommitReview` still omits typed base | Add base to owning project contract and consumers |
| First gate passed despite scheduler-sensitive final-with-pending behavior | Repaired gate at `53a219b`, merged `703005f`; it uses a later final response | Require explicit barriers; do not claim the original final-with-pending branch is covered |
| Compile latency fell while coordination remained costly | Wave-8 observations report warm compiles in seconds | Report compile and coordination costs separately; no causal speedup claim from different tasks |
| Automatic review was proposed as new composition | Existing `checks/review-continuation.hs` already composes settlement, exact submission evidence, retained review and repair | Reuse and validate before adding another implementation |

Historical sources: `plans/wave8-observations.md`, harness
`docs/exomonad-friction.md` wave-8 interview and the referenced commits. Reports
remain reports until corroborated by source or retained traces.

## Hypotheses and measurements

| Hypothesis | Opportunity | Outcome / counterexample |
|---|---|---|
| Typed provenance removes avoidable review repairs | Exact-commit review admitted | Assigned base/candidate, checked HEAD, blocked invalid packet or wrong checkout; distinguish a malformed packet from failed checkout materialization |
| Event-driven waiting reduces empty polling without losing work | Parent has pending work and a registered completion route | Empty rounds; actionable event to parent action; lost, duplicated or unconfirmed notifications |
| Behavioral acceptance improves efficacy | Candidate and independent review | Actual failure cases considered, semantic repair rounds, deterministic barriers, integrated executed checks |
| Authored coordination removes recurring model work | Candidate settles with sufficient evidence | Automatic request admission and reviewer result; model interventions, source mismatch, unavailable reviewer and extra state complexity |

Use existing trace/transcript and Git evidence. Record actor/request/time,
source and prompt exposure, opportunity, outcome and evidence reference. Classify
held/missed/unknown/pending; unexercised is not success. No new periodic actor
polling or runtime instrumentation. Operator steering is recorded as an
intervention. Different tasks and concurrent changes prevent causal attribution
from total duration alone.

## Capability frontier

| Ideal scenario | Available now | Gap to investigate |
|---|---|---|
| Express dependencies once and advance ready work without routine model turns | Typed settlements, persistent record actors, WorkSink, retained review/repair composition | Discoverability and production use of the authored flow; automatic admission must preserve exact source and failures |
| Child asks a typed question; parent chooses code, semantic judgment or model attention | Typed requests, effects, Jev, parent notifications | A uniform authored decision route and its authority/lifetime contract; do not invent another scheduler |
| Runtime observations work for arbitrary project contracts | Generic actor/request identity and typed input, authored Display | Status currently parses the project-specific taskSource spelling; investigate authored typed observation metadata instead of teaching Rust each project record |
| Run inspects evidence and tests a policy change next wave | Source reload, typed tools/hooks, trace artifacts and RSI prompt | Reliable evidence selection and exposure tracking; historical log availability is not yet a typed query interface |

These are directions for experiments, not assertions that all parts work today.
Harness adoption may replace coordination mechanisms; avoid growing a second
durable store or long-lived compatibility framework around the current backend.

## Process notes for the eventual RSI pattern

- Separate confirmed repairs, falsifiable hypotheses and frontier experiments.
- Reconcile already-landed interventions before proposing another fix.
- Check for existing capability before treating awkward usage as a missing primitive.
- Keep task success and execution quality as distinct outcomes.
- Specify disconfirming evidence and unknowns before observing the run.
- Prefer one owner for each mechanism; authored policy composes runtime primitives.
- Use interviews to explain choices; verify factual claims against artifacts.
- At close: report product outcome, hypothesis verdicts, retained/revised changes,
  new possibilities, and the next proposed wave. Specify the reusable RSI pattern
  from the actual iteration, including deviations and intervention costs.

## Execution record

- Preparation started; no new wave launched yet. Existing wave8 host is retained.
- Existing item-2, item-13 and live adapter inference holds remain in force.
- No edits to the user-owned `plans/harness-adoption.md`.

### Preparation findings

- The review skill used three-dot diff, which can choose a different merge base.
  Updated guidance requires ancestry and an explicit base-to-tip diff.
- Runtime status extracts assignment_base from rendered taskSource only. It does
  not understand project CommitReview. The new typed packet/activation is the
  evidence for this trial; status may say none. Extending project-specific string
  parsing in the runtime is deferred in favor of a future typed observation seam.
- The automatic review fixture captures its initial Task. Live amendments need
  a new flow after pending work settles; the experiment must not route a repaired
  candidate under stale accepted decisions. Its recipe uses keepWork; a live
  flow must install notifyWork or it will retain results without waking the owner.
- The wave-8 interview conflates first-request queueing with amendment 4. The
  latter explicitly tests a follow-up arriving during finalization and final
  reply seen/unseen provenance. The wave assignment now preserves that distinction.
- Independent Sol review inspected the typed provenance diff and continuation
  composition; no build was claimed by that reviewer.

### Check preparation evidence

- Direct deployed checks initially used cold compiler endpoints; supervisor
  interrupted both own process trees after repeated cold compiles. Incomplete,
  not failed assertion evidence.
- Reusing the wave8 daemon with the deployed binary reached a startup failure:
  MissingPreparedTop Project.Watchdog.coreHeuristics1, before provenance assertions.
  Log: /tmp/rsi-review-provenance-warm.log. Cause not established; no daemon restart.
- Switched to the repository's matched local build/check wrapper, which reused
  the persistent battery daemon. This is an explicit build/exposure change.

### Resource cleanup and launch policy

- User explicitly authorized stopping all unused processes. Stopped completed
  wave8 and two old TUI hosts through exomonad stop, the persistent test daemon
  through just daemon-stop, and an orphaned fixture daemon. Source, worktrees and
  evidence retained. Available RAM increased from about 5 GiB to 23 GiB.
- Run launches previously left compiler pool size to available memory. Preparing
  explicit --workers 1 and --rss-ceiling-mb 7168 with the owning launch test,
  so freeing memory cannot silently increase a run's worker count. The RSS
  threshold rotates between requests; it is not a hard process memory limit.
- The first private check daemon inherited a 2 GiB default budget at startup
  under pressure and rotated on almost every compile. It reached real candidate
  activation and correct base/HEAD assertions. A fresh 7 GiB one-worker check
  with existing memo/recovery diagnostics tests the intended warm configuration.

### Verified preparation

- Workspace `c6d3104` published on integrate-ws2, pinned/template-synced in
  Tidepool `522dad4fb`; typed review provenance recipe: **4/4 passed** on fresh
  one-worker, 7168 MiB threshold daemon. It executed the published skill call and
  acceptance snippet, checked actual reviewer HEAD and distinct base/tip.
- Launch policy `20887af2d`: final focused facade test **1/1 passed**, 485 skipped;
  Rust formatting (edition 2021), template byte equality for all seven changed
  workspace files, and diff checks passed. No full gate claimed.
- Redundant cold check stopped after three assertions once the warm check passed
  all four; no result attributed to its unexecuted remainder.
- Detailed logs retained under target/tidepool-test-runs/rsi-iteration-1/.
  Earlier retained-daemon failures are unresolved compiler-state evidence, not
  reproduced by the successful fresh warm check. No speculative engine fix landed.

### Automatic review preflight verdict

Project.RoutingChecks.automaticReview failed before its assertions, on generated
wrapper lines: Project.Routing.WorkEffects is not exported, and
Control.Monad.Freer.State.State is not imported. Exact log retained as
target/tidepool-test-runs/rsi-iteration-1/automatic-review.log. This corroborates
the earlier wrapper type-pin defect. Wave 9 uses ordinary reviewed delivery;
automatic review is blocked, not exercised or successful. No fixed-whitelist
workaround was added. The frontier question is how the workbench preserves
inferred types without emitting names unavailable to authored source.

### Wave 9 launched

Run `4778f12e-433e-4dcc-b686-cacff9c81bb2` is ready in tmux wave9. Brief submitted at 2026-09-25T17:56:23.620100+00:00.
See [launch record](wave9-launch-record.md). Product outcome and live prompt
trials are pending; automatic review is preflight-blocked.
