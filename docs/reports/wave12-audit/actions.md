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


## Second scheduled check — 2026-09-26 05:11 UTC

Wave13 remains active, not accepted. Run
`8d739b47-74e4-47e1-8bd2-7989bdda729e`; checkout
`/home/inanna/dev/exomonad-harness-runs/wave13`. Launch owner repaired runtime
`cargo run` into a prebuilt-binary launcher at harness `ef862550`; root's
standalone acceptance `5ab5fea` remains unverified at this check. Independent
review actor8 is working. Preserve wave12 demo hostPID3202992 on port4600.

Two independently traced infrastructure failures prevented intended parallelism:

- Child actors3–7 encountered `tmux new-window` occupied-index errors. Their
  memory admission was immediate; this is not a physical-memory diagnosis.
  Atomic insertion already existed in held branch5e61feecc but was excluded
  from launch main. Isolate and test that fix before integration. A proposed
  duplicate-flag commit05f276810 was caught in review and must not be merged.
- Root's focused jobs `fe09bae8-66e5-4028-978f-7687acf35563` and
  `47cf4320-1d6b-4e80-a23f-897ff29c2d3f` requested8GiB, the entire general
  command pool. The preserved demo job `2e996f94-9093-49f7-ba56-a11814d5d00e`
  holds1GiB. Both jobs therefore remained queued and were cancelled without
  execution or output streams. Journal sequences54651/54840 are admissions;
  after cancellation54757, a queued4GiB job allocated54761, started54765,
  completed54772. Use a realistic smaller request, not implicit overcommit.
  Audit existing queue observations for a typed admission-blockage explanation.

A completed-call timing sample at05:17Z has24 calls over10s. Four longest
completed observations are about300s, with compiler time0–853ms and command
wait about300s. These are not evidence of five-minute compilation. Retained
raw derivation: `/tmp/wave13-slow-completed-calls.json`; owning run log above.
Backgrounded cells are call observations, not command completion evidence.

Direct native `codex queue` to the root failed with a session-metadata error;
operator delivery is being investigated before claiming the root received the
reservation diagnosis. No running host or daemon was restarted. Next scheduled
check remains22:56PDT through `tidepool-rsi-supervisor.timer`.


Follow-up at05:21Z: tmux insertion fix integrated as `fece13978`, reviewed from
isolated `fbdeaedff`. Exact main check
`just test-lib exomonad-node 'test(=tmux::tests::dedicated_socket_session_has_exact_create_and_kill_lifecycle)'`
passed1/1 (96skipped), including eight concurrent actor windows. No live host
was rebuilt/restarted. Reversible live mitigation installed on **wave13 only**:
session-local `after-new-window` selects `wave13:{end}`, and current selection
was moved to the last window. No prior hook existed; pane IDs/processes stay
unchanged. This changes visible focus and is a temporary mitigation, not the
atomic production fix.

Operator notification25 to root carries the memory-reservation diagnosis:
admitted and retained, not yet presented. Reviewer actor8 received the separate
preparation-path review question (notification2, Presented). Root again queued
an8GiB job, so active-turn delivery is under investigation. The queue-observation
implementation is assigned to `audit_abstractions` in isolated branch
`perf/command-queue-observation`; use the existing owner and per-job observation,
preserve FIFO and protected bypass, and expose no other-run identities.
