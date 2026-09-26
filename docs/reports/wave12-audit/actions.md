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


## Third scheduled check — 2026-09-26 05:56 UTC

Wave13 is still active; no successor launched. The root incorporated the
reservation diagnosis and used4GiB. At harness `cf19ed4`, helper executions
passed server8/8 and standalone1/1; helper source/notes retained as `fbf08e6`.
This proves execution after publication/import. It does not yet prove reuse by
two distinct actors or reduced model/tool calls. Root notes missing cleanliness
metadata (`working_tree_status: null`), so its strict helper verdict remains
False despite passing assertions. Investigate runner provenance separately.

Reviewed launch `ef86255` integrated as `d39a7ac`. Real preparation executed
web14/14 and browser journey1/1 but then failed artifact lookup because
CARGO_TARGET_DIR differs from the assumed target path. Independent review
rejected repair19a6818: failed preparation could leave an older default binary
launchable. Further repair is in progress. Standalone production-binary test
already passed unrelated-cwd login, commands/child messages, missing assets,
SIGINT reopen and SIGKILL reopen with isolated port/data. These are not yet a
successful final integrated preparation/script launch. Keep exact acceptance
boundaries and update the handoff only after final checks.

Correction to prior monitoring inference: repeated graph observations with
`awaiting_effect: command job` and the same provider turn did NOT prove one
indefinite call. Completed timings show bash ending05:23:18 after301209ms and
write_stdin ending05:28:21 after300247ms. They were sequential bounded waits.
Current sample retains40 completed call observations over10s,16 added since
05:17; `/tmp/wave13-slow-completed-calls-0556.json`. Longest new waits again
spent about300s on command observation, under1s compiling.

Queue visibility f53900607 passed independent review and integrated as
`39bd8b1a3`. Owner/RPC/protocol/Haskell/hosted receipt tests passed in its worktree;
integrated focused checks are running. No scheduling change or cross-run IDs.
An old shared resource service degrades to generic queued; it has NOT been
restarted. Deployment and generated-effect integration checks remain distinct.

Managed-root branch54d1d479e remains held. Scoped review found changed-parent
symlink import risk and uncancellable/nonregular authored-file reads during
root service preparation. Repairs and focused reproductions assigned separately.
No broad integration or live-host change authorized by review alone. A separate
assignment traces launch errors being erased into generic host-shutdown
cancellations, which hid wave13's tmux diagnosis from its parent.

Protected demo still listens on4600 as hostPID3202992. Timer remains active;
next tick23:41PDT. Existing run/log/source/pin identities remain unchanged.


### Integration rollback — 06:06 UTC

Queue diagnostics are **deferred**, not deployed. Integrated tests passed2/2,
but at06:04 the running wave13 compiler rejected `Tidepool.Command` because
its old generated `Tidepool.Effects.Core` lacks `CommandQueueWait`. The live
worker reads the mutable Tidepool Haskell library; changing the schema/library
under it broke compatibility despite focused checks passing with a matched new
worker. Reverted integration39bd8b1a3 as `f8483da2c`; the reviewed implementation
remains `f53900607` on its isolated branch. No shared daemon was restarted.
Root notification and actual next-cell recovery verification are pending.
Future schema/library integration must wait for active runs or use an immutable
matched runtime-library snapshot; treat this as an owning-source isolation gap.

`just fixtures-check` stopped at stale Suite oracle fingerprint; it did NOT
pass. Native GHC regeneration changed only the fingerprint, no expectation or
payload digest. That generated edit was removed with the schema rollback.
The original fingerprint still fails current input validation after rollback;
retain run evidence `target/prepared-corpus/run.tF5pv5` and investigate the
fingerprint input set before a later release gate. No oracle values were edited.

Managed-root scoped follow-ups committed7b16899bb (reject authored FIFO/nonregular
entries) and a8f61b41a (reject changed parent symlink before artifact import),
with focused tests. Cancellation-aware authored copying still underway; a
fully adversarial concurrent path-swap race is not claimed solved.


At06:08 recovery is verified: operator explicitly imported Tidepool.Command
and evaluated Cmd.GiB4; root Haskell cells committed06:07:30 and06:07:51.
Notification33 was Presented. No command or restart was needed for that proof.
Harnesshandoff ee5a0da reports completed local product checks and leaves the
ordinary-host lifetime gate open. Verification agent is executing only that
isolated host gate; no port4600 migration or run teardown has begun. Root and
reviewer interviews are retained in the run checkout. Cleanup skill loaded;
terminal and released remain separate facts. New wave remains gated.


### Product accepted and run stopped — 06:15 UTC

Wave13's external ordinary-host gate passed: copied prepared binary/assets,
launch from `/` via user systemd, host-visible PID and cgroup independent of
actor namespace, login/WebSocket/echo/reconnect, exact SIGKILL and persisted
reopen, exact SIGINT exit0 and port release. No Cargo at runtime. The initial
unit lacked Bash in its service PATH; successful units supplied the standard
host PATH. Evidence is tracked as harness `docs/wave13-host-gate.json`, with
hashes, source and invocation IDs; no secret retained. Acceptance docs951ab0d
and all reviewed wave13 work merged to harness master `88ff710`. Production
paths are byte-identical to tested d2de055 (verified Git diff). Original demo
hostPID3202992 remains on4600. Worktree helper deletions remain preserved.

Root retirement returned StoppedNow. Wave13 host/compiler stopped and its tmux
session is gone. **Full resource release is not proven:** old failed launch
actors3–7 retain process/workspace custody because supervisor socket lookup
returns ENOENT. Shutdown status remains failed/retained; do not delete those
resources or equate absent processes with a complete release receipt.

Native launch-cause preservation integrated as `3cbb987d9`; failure and ordinary
shutdown focused tests passed2/2. Tmux insertion fixfece13978 remains integrated.
A further provisional cleanup patch in `scoped-never-started` is UNSAFE and
uncommitted: tmux3.7c client.c maps lost-server IPC to normal exit1, so nonzero
status/empty output cannot prove no child started. Keep the worktree as evidence;
never integrate its never-started classification. A reliable launch fence or
receipt needs a separate design; generic ENOENT must not become a release claim.

Oracle owner fix integrated `42db232c0`: exclude generated CBOR artifacts,
NUL-delimit paths and expose hashing errors. Synthetic regression passes1/1;
retained-manifest check passes. Native GHC regeneration changed only script's
self-dependent fingerprint, not expected values/payload digest. This explains
why restoring the old Haskell library alone did not repair the stale check.
The full corpus execution previously stopped at the oracle gate; it is not
reported as a fresh full pass. Matched `just exomonad-build` now passed.

Wave14 brief is harness36cdebf: first typed pass-through before-request hook via
existing Provider/Engine/Store and deterministic browser consumer; three Luna
obligations plus independent review, and a genuine two-actor composed helper
trial. Isolated branch rsi/wave14 atfb38791, workspace pin98aef85. Only that new
worktree's submodule gitfile was made absolute using the existing normalization
contract. Host Git status is clean; actual actor-namespace check is pending.
Runtime build is42db232c0, catalog39. Workspace check/launch still pending at
this entry. No second working wave overlaps wave13; old retained resources and
wave12 demo remain untouched.


### Wave14 active — 2026-09-26 06:36 UTC

Matched incremental build and workspace check passed. Launched Sol Medium
root in `wave14`, run `08a7d4c5-4821-4887-8a07-42470a08029b`, thread
`01a0dc67-2aed-7eb0-95d5-15dedc7ef92f`. Runtime launch checkout15306f7c4
(production42db232c0), harness admission5c398bc, workspace98aef850d7be.
Worktree `/home/inanna/dev/exomonad-harness-runs/wave14`; current NEXT.md
is there, not the completed-wave13 NEXT on harness master. Log is that
worktree's `.exomonad/logs/<run>.log` and adjacent JSONL. Full launch facts
are in its `docs/wave14-launch.md`.

Initial task visibly started06:31:05 after a second Enter; no duplicate task.
Root established compiling contract81ef34ce and admitted three children
06:34:40–55. Actor-view Git status succeeded and reported dirty helpers;
unknown cleanliness is not silently accepted. Helper publication/import and
root's focused1/1 test are observed, but two-actor reuse remains pending.
Luna audited the initial five minutes. Jev403 RBAC failures appear across
actors; the shared breaker returns circuit-open abstentions and tools still
commit. Investigate authorization separately from credit exhaustion; do not
print credentials or treat hook abstention as successful semantic judgment.

Preserved wave12 demo still HTTP200 on4600. Wave13 has no live host/compiler,
but uncertain old custody is retained. Timer remains scheduled23:41:18 PDT
(06:41:18 UTC). No overlapping wave. Keep runtime Haskell/schema edits isolated
while these hosts run; no shared daemon restart.

Initial audit detail (06:31:05–06:36:05 UTC): completed Haskell cells above10s
were12,607ms (compile11,070ms, Jev1,063ms, execution135ms) and22,189ms
(compile10,512ms, Jev1,037ms; remaining time not attributed by this sample).
The2,870ms cell is below threshold. Actor2's rejected cell06:35:15 supplied
an incomplete DesignQuestion constructor, leaving required fields unapplied;
retain as model-facing API ergonomics evidence. Actor2 had16 captured Jev
effects:2 actual HTTP403 RBAC denials and14 circuit-open responses without a
backend request. These counts are actor-specific, not a whole-run total.


### Scheduled supervision — 2026-09-26 06:41 UTC

Wave14 remains active; no new wave or teardown. Root checkpoint names Provider/
Store2a9852cc (focused checks reported1/1 each), independent reviewer actor5,
Engine7b5693a6 (compiled only, retained owner asked for cancellation/failure
repairs), and independent browser acceptance actor4. Review correctly declined
provider-only forwarding tests as proof of Engine persistence/correlation.
Root is incorporating review and expected-red work; no product acceptance yet.

Read-only Jev investigation counted10 actual provider RBAC403 responses and120
local circuit-open refusals through recovery06:40:26, followed by58 successful
responses at the audit snapshot. Source confirms one shared client admission
before send and one recovery probe per30s. Logs establish neither credit nor
credential cause. Existing breaker worked; future telemetry should distinguish
local circuit-open observations from provider HTTP responses without a new
service owner. No credential inspection, network probes or restart performed.

Helper experiment: actor3 reported publication after reload_helpers, then one
composed cell issued3 command effects but selected/executed0 tests (expected1);
this is a failed selection, not a passing test. Its Jev judgment failed during
403 interval. Actor2 reported the root's 4-to-3GiB dirty helper snapshot stayed
in its own checkout despite the root reverting locally. Reviewer reported an
import rejection with no command effect, followed by direct-script fallback.
These distinguish file inheritance, publication, invocation and actual reuse.
Supervisor notification18 admitted to root: experiment does not require exactly
4GiB; realistic3GiB plus exact helper/source evidence is valid. No instruction
to rerun completed product tests merely to satisfy the experiment. Admission
is not evidence that the root has read this notification.

One-line task-prompt repair8e4f7720cc47aab5972b173a2d57487870f4538d is isolated
in harness branchrsi/question-prompt, worktree rsi-question-prompt. It supplies
the six DesignQuestion fields previously available only in specialist prompt;
compared with pinned Project.Types, diff check passed. No executable snippet
or runtime schema added. Awaiting independent review; leave live wave unchanged.

Sol investigates runtime stdlib isolation in a separate worktree: dev facade
embeds an empty bundle and falls back to mutable checkout library; frozen
workspace hashes but does not copy it. Require protection against mid-run edits
and distinguish admission of an already-mismatched old binary. No live library
changes or shared daemon restart authorized by this investigation.

Protected demo HTTP200 at this tick. Timer next00:26:19 PDT (07:26:19 UTC).
Dirty NEXT/helper files and retained cleanup resources remain untouched.
