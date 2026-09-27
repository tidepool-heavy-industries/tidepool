# Wave18 wind-down and retained lessons

Inanna explicitly ended wave18 incomplete. Run
`29e61b63-2bc9-45d5-b5d1-b67e20918bea`, session `wave18`, stopped
2026-09-27 UTC. Supervisor handoff/WIP documentation commit: `9e3f1be` in
`/home/inanna/dev/exomonad-harness-runs/wave18` (branch `rsi/wave18`).

## What is preserved

Four final interviews were read before the whole-run stop:

- [Standalone owner 10](wind-down/wave18-final-actor-10.md), final interview
  committed by supervisor as `ebbc726` after its interrupted commit left an
  index lock. Lock bytes were renamed/preserved after confirming no Git process.
- [Operator owner 11](wind-down/wave18-final-actor-11.md), `2cd840a`.
- [Producer implementer 44](wind-down/wave18-final-actor-44.md), `1001be7`.
- [Browser worker 66](wind-down/wave18-final-actor-66.md), `61ab544`.

Original reports remain in their original worktrees. Copies here are evidence,
not integration of their product code. The [source inventory](wind-down/worktrees-after.json)
records full OIDs and paths. All 18 observed actor-bound worktrees remain present;
none was deleted. Root's dirty NEXT, execution notes, critique and chronology
were preserved in its WIP checkpoint together with the supervisor handoff.

Root acknowledged the wind-down, but a further Haskell control call remained
pending. Its new final interview was not completed; the supervisor read and
retained its extensive existing `wave18-automation-critique.md` and design
follow-up. Do not describe the new interview as answered. Old interview text
about continuing acceptance is superseded by the explicit stop instruction.

## Release evidence

`exomonad stop --run-id 29e61b63-2bc9-45d5-b5d1-b67e20918bea --session wave18`
returned zero. [Release evidence](wind-down/release.json): service inactive/dead,
MainPID 0, Result success; tmux absent; [all inventoried processes absent](wind-down/processes-after.json).
The host resource snapshot reports active 0, retained allocations 0, cleanup
failures 0 and process count 0. The run-owned compiler stopped with the session;
no unrelated daemon was restarted or deliberately stopped.

An earlier scoped actor-stop call did not return a terminal receipt before
whole-run shutdown and subsequently returned 503/channel closed. We do not claim
individual StoppedNow results. Service/process/resource observations establish
whole-run release. `status.json` still says phase ready: that phase is stale.

## Concrete lessons and next actions

1. **Expose a callable browser-check procedure with resource settings.** Actor66
   avoided runBrowserCheck because its assignment mandated the shell invocation;
   actor44 could not find the documented helper in its bound source. Actor11 found
   completion/read ergonomics awkward. Deliver helper source and validate its
   callable presence before referencing it; let the helper accept the exact
   candidate, filter/count and memory/build settings. A prose menu is insufficient.
2. **Review packets must retain base and response owner.** Root and Operator
   report stale-base review scope and references to another actor's response
   handle. Use original typed responses/checkpoints and scope preflight. Existing
   reviewed-checkpoint work addresses part of this; verify adoption next wave.
3. **Preserve immutable model-request evidence and failed-run artifacts.** Mutable
   Store items and chronology were mistaken for as-sent input. RecordedReplayTurn
   is the identified boundary. Temporary DB/capture cleanup then removed the
   evidence for the next recovery failure. Retain failure artifacts with a bounded
   policy rather than rerun merely to recover them. No new durable-log owner.
4. **Reject accidentally function-valued task admission at the task workflow
   boundary.** Explicit Task annotation corrected a partial helper application.
   Generic input APIs need not forbid functions globally. Owner/writer disagree
   about request-number attribution; retain exact commits without inventing a
   resolved chronology.
5. **Make stop/cancellation progress observable and bounded.** Interrupted root
   Haskell produced repeated NotSleeping cancellation responses. The pinned TUI
   queued new input until hosted-call reconciliation; it eventually recovered
   without restart, then another wind-down control call was slow. Source audit:
   `vendor/codex/codex-rs/tui/src/host_dynamic_tools/cancellation.rs` treats
   NotSleeping as pending with 100ms retries, and event_dispatch queues input
   behind the active dynamic tool. The underlying delayed settlement cause is
   not established. Repeated steering did not accelerate it. Retain this as a
   focused lifecycle investigation, not a reason to keep a finished wave alive.
6. **Explicit wave termination beats indefinite integration drift.** Parallel
   component work delivered useful code, but the serial recovery tail kept the
   wave nominally active. Next waves need an explicit incomplete handoff at their
   stop boundary, exact remaining owner, and acceptance separate from partial
   component deliveries. Also publish stopped phase through the run-status owner;
   persisted ready phase must not masquerade as live work.

## Remaining product boundary

Unreviewed repair `23f40a4` is retained; its single browser run matched/executed
one test and passed zero, exposing `UNIQUE constraint failed:
requests.parent_id, requests.branch` during process-loss reopen. The exact
Engine/Store recovery-continuation invariant needs investigation. Do not weaken
that constraint or infer persisted Interrupted from a report without the DB.
No further product tests or repairs were run by the supervisor during shutdown.
Wave19 must allocate this unresolved obligation explicitly, alongside any new
feature trees; wave18 will not be resumed to satisfy a launch checkbox.
