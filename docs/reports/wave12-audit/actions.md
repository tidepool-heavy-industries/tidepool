# Wave 12 audit action queue

These actions are authorized by the user. Audit reports describe an observed
prefix of the run, not a final product verdict. Verify evidence before promoting
a recommendation into a standing rule.

## Implementation and validation

| Work | Owner / status | Acceptance evidence |
|---|---|---|
| Responsive compiler control under one busy worker | main 132784eeb + bc908ecfe | Typed BUSY retries same endpoint without rebind; six focused tests and Clippy passed |
| Reuse recovered reachability in projection | main 6f4ea972f | Independent review, full fixtures gate (Suite 692/0 plus ancillary cohorts), focused recovery and matched host build passed; live speedup unmeasured |
| Cancel fork imports without fallback launch | cancellation branch through 5e61feecc, awaiting review | Build, pre-cancel, in-flight copy kill/reap and captured-source cleanup tests passed |
| Failed admission releases provisional resources | 5e61feecc, awaiting review | Concurrent tmux insertion tested; provisional Git receipts deliberately retained by owner contract |
| Managed root baseline, private overlays and explicit integration | managed_root_integration, branch through 15a8e1edd plus WIP | Source/authority tests passed; artifact mounts, cancellation, frozen-source parity and descendant acceptance pending; excluded from wave13 |
| Clear producer contracts and completion evidence | harness master 6119ae5 | Clarification, expected-red prerequisites, retained checkout and pending-update guidance corrected; diff checked |
| Reconcile audit evidence and action ownership | root, active | Correct actor identities and resolve conflicting helper-import claims before treating reports as settled |
| Jev shared-service breaker | main 136790a14 | 14 focused tests passed; facade compiled; no running host changed |
| Stale notice filtering and accurate queue logs | main a2298a42e | Five focused tests passed; shared observation checks; presentation remains separate |
| Overlap independent resource-release waits | main 2e51dcf51 | Focused ordering/failure tests passed; three delayed releases measured123ms serial vs42ms batched; separate groups remain serial |

## Investigation and experiments

- **Stale compile snapshots:** audit_abstractions investigates locked fallback,
  bounded retries and source sequencing. Preserve compile-view validation;
  discuss fairness, consistency and cancellation before changing the strategy.
- **Coordination helpers:** queued. Try small notebook compositions using the
  existing typed response/review surfaces, including a compact settlement
  collector. Compare actual calls, output and useful consumers; discard trials
  that do not repay their complexity. Multiple trials are welcome.
- **Implementation clarification:** the node messages its parent with the
  concrete question. Continue independent work when available. Do not treat a
  legitimate question as a failure or require a complete contract before every
  fork. Parent decisions and source changes still need explicit incorporation.
- **Repeated unavailable Jev judgments:** breaker implemented at the existing
  shared JevClient owner. Account failures pause provider calls with one recovery
  probe; callers receive explicit failures, never invented judgments.
- **Command handle failure:** traced to a mistyped ID, not registry loss. Audit
  handle reuse/transcription ergonomics before proposing lifecycle changes.
- **Broader Luna audits:** compilation, context cost and artifact amplification
  completed. Counts of native turns must not be presented as model rounds. See [experiments](experiments.md)
  for bounded trials and their acceptance evidence.

## Dispatch policy

Use isolated worktrees for implementation. Reuse agent slots as tasks finish.
Keep live hosts and their compiler daemons untouched. Run focused checks during
parallel work and the necessary broader gate at integration boundaries.

## Scheduled supervision

User authorized recurring 45-minute RSI investigation, interview, completed-run
teardown and next-wave launch while away. Installed user-systemd timer:
`tidepool-rsi-supervisor.timer`; first wake 2026-09-25 21:26 PDT, then every
45 minutes. It queues a message to supervisor Codex thread
`01a0dad6-69ba-7ce0-aed3-cd9192d82ff5`; this requires the machine and attached
Codex session to remain available. Coalesce delayed ticks, never overlap waves.
Inspect `systemctl --user list-timers tidepool-rsi-supervisor.timer` and
`journalctl --user -u tidepool-rsi-supervisor.service` for scheduling/delivery.
Stop with `systemctl --user stop tidepool-rsi-supervisor.timer` when the user
ends unattended supervision.

Preserve the live browser demo and its Tailscale route. Teardown concerns the
completed Exomonad run's actors/resources; inspect cleanup ownership and receipts
before stopping anything. Launch the next Sol-root wave only after its brief and
required checks are ready, with bounded parallel work and one experiment. Timer
expiration alone is not a launch gate. Current wave12 product is delivered;
its root retrospective has been requested, and implementation work continues.


## First scheduled check — 2026-09-26 04:26 UTC

Wave12 root interview committed in harness `cce811a`; 55 completed root cells,
23 over10s are recorded in completed-root-latency.md. All seven worker resources
released. Host retained deliberately: demo hostPID3202992 is under the root
command namespace/cgroup, so stopping the host risks the protected demo.

New verification recipe is harness `c6a5916`: pinned Node24, npm ci/check/test/build,
then existing focused browser runner. Clean-source run passed14web tests and
1browser API journey. Real-browser UI verification remains a distinct boundary.

Wave13 brief `7445af3` and assignment `b6e5a14` target portable standalone browser
operation with isolated startup/reopen evidence and a focused-gate helper trial.
Initial launch against the old harness path correctly refused its live-owner
lock. Retry uses `/home/inanna/dev/exomonad-harness-runs/wave13`, branch
`rsi/wave13`, leaving the protected demo and original checkout untouched.
Launch log `/tmp/rsi-wave13-launch-isolated.log`. No root task is considered
started until the launch and initial-message receipt are recorded.

Wave13 launch succeeded: run `8d739b47-74e4-47e1-8bd2-7989bdda729e`, tmux
`wave13`, root `%547`, thread `01a0dc01-bca8-7b11-97b9-06b79d9d51f6`.
Initial instruction executed; root authored contract `b43cdb1` and began helper
publication. Harness `docs/wave13-launch.md` records exact source and pin.
Read the **run checkout's** NEXT/logs on future ticks. A Luna observer is
sampling startup. Next timer remains 22:11 PDT.
