# Wave closeout and comparison with NEXT.md

Source checkpoint: Tidepool main `5ba83eb2bef9ba32d9303b08d7d7a779e2de0323`.
The original wave plan remains in Git history. [NEXT.md](../../../NEXT.md) is
now the current implementation handoff; the goal comparison below records the
previous wave's objectives and must not be read as current dispatch instructions.
The user explicitly accepts partial integration. The next phase is one dedicated
implementation session, not another Shoal dogfood tree.

## Goals versus delivered source

| Original goal | Current result | Remaining obligation |
|---|---|---|
| Separate execution from TUI, one mounted native service per actor | Partial. Native controller/observer support is delivered and pinned at `600be9df`; Tidepool still uses the legacy interactive launch path. | Implement the persistent native client/controller and move hosted-call forwarding out of the TUI. |
| Exact custody before bootstrap and safe lifecycle ownership | Substantial reviewed implementation merged: recursive/sibling bootstrap, admission/seals, durable inbox fences, retained process/socket/build ownership, canonical actor/endpoint construction. | Compose actual native/external cleanup and final custody settlement; component evidence is not whole-service quiescence. |
| One connection for assignment, amendment, notification, wake and cancellation | Typed notification and receipt infrastructure exists; ordinary request/reply/watch work remains. Production notification send explicitly rejects with Unavailable. | Wire the native controller, repair actual amendment transport and prove distinct correlated intent semantics. |
| Artifact-first run map | Bounded inventory, metadata, time windows, CLI and partial-input handling are merged. The bounded per-response usage reader is now wired/exported and tested. | Connect that reader to the report/CLI; usage and acceptance are still Unknown in RunMap. Unrecorded parent/source/review edges must remain unknown. |
| Reusable Haskell evidence helpers and improved working practice | Helpers, coordinator examples, focused recipes, documentation fixes and reviewed coordination prompts are merged. | Validate improvements through later use; no controlled cost/cache/latency superiority was established. |
| Fresh acceptance of the integrated native/Tidepool pair | Independent preparation and host-pairing acceptance completed; closeout reran focused integrated host checks. | The decisive mounted no-TUI/observer/sibling-prefix/amendment/notification/reconnect/retirement matrix has not run. |
| Selected-context small workers | Not started in this wave, consistent with its explicit service-acceptance gate. | Leave gated until the service product passes acceptance. |

The former service-migration outcome was incomplete at this checkpoint. The
[native client handoff](../service/native-client-handoff.md) and
[acceptance matrix](../acceptance.md) retain its historical design and evidence.
That migration is not a current requirement: the selected implementation keeps
the existing interactive Codex TUI path and fixes concrete failure behavior.

## Integration and preservation audit

Merged preparation `1caf11e5`, service handoff `2dc3daf1`, repaired canonical host
pairing `02a2fe31`, native pin/build `ecff53c2`, coordination prompts `526c02df`,
bounded usage implementation `9ba36ef4`, and late service evidence `f28aafb0`.
The formerly unsafe pairing candidate survives in Git history but its blocker is
superseded by the merged canonical-construction repair and negative regression.
Closeout commit `51926e43` adds the requested usage dependency/exports and repairs
an old overflow fixture to supply individually valid records to the stricter parser.
The CLI consumer remains an explicit partial-work boundary.

Audited all 50 local branch tips with commit timestamps at or after this run's
2026-09-06 19:17 UTC start. All are reachable from main. All 50 corresponding
registered checkouts were clean. The audit also checked the 25 workers still
visible before shutdown; it found the late service receipt and merged it.
This establishes no omitted committed branch work in that audited set, not an
archive of arbitrary resident values or every scratch/build artifact.

The separate Codex repository contains fallback fix `72d1628c76` on
controlled-execution-observer: 60-second settlement attempts and disabling hosted
tools instead of fatal TUI exit. After this checkpoint, the user authorized its
pin: that exact commit was pushed to the fork and Tidepool's flake source/lock
updated. Lock resolution and contract derivation evaluation passed. The earlier
native build evidence remains specific to `600be9df`; the new pinned Nix binary
has not been built or deployed. Tidepool's closeout commits remain local.

## Verification

- At the code state committed as `51926e43`, `NEXTEST_TEST_THREADS=1 just test-lib
  tidepool-agent 'test(rollout_usage)'`: 18 passed, 82 skipped. The first run caught
  the invalid old overflow fixture; the corrected run passed.
- At `51926e43`, `NEXTEST_TEST_THREADS=1 just test-lib tidepool
  'test(actor_host::hosted_retirement::tests::) | test(actor_host::prompt_catalog::) |
  test(custody_precedes_first_bootstrap_worktree_use_for_two_siblings) |
  test(http_actual_resident_seal_identity_late_dispatch_and_completion)'`:
  15 passed, 140 skipped; nextest `04c0d532-50da-4e6d-9099-9f0e74bce9d6`.
  Host library and its dependencies compiled; private daemon teardown reported.
- `nix develop --command cargo fmt --all -- --check` and `git diff --check` passed.
- Earlier independent [preparation acceptance](../service/root-preparation-acceptance.md)
  records 19 checks at `62e99ec6`; [canonical pairing review](../service/canonical-host-pairing-review.md)
  and the final section of [service convergence](../service/convergence-checkpoint.md)
  retain their exact nine-test runs and fixture repair evidence. These are separate
  revisions/reruns, not test counts to add into one acceptance total.
- Codex fallback: 13 focused checks passed; full TUI run had 4,262 passes,
  20 failures elsewhere and 6 skips. No clean full-suite claim.

Local closeout logs are `/tmp/tidepool-wrapup-{usage,host,fmt}.log`; existing
specialist evidence remains in its original worktree target directories.
No full workspace battery or mounted service/provider canary ran during closeout.

## Runtime state and next dedicated run

The old host was stopped through SIGINT and its normal forest/application shutdown;
its status records Exited. The run compiler was then signaled. Worker tmux panes
closed. This was not a new-host deployment or a proof of all-resource cleanup.
Branches, worktrees and saved Codex conversations remain. Live Haskell bindings,
pending handles and unrecorded reasoning are not recoverable from this handoff.
Bounded terminal handoffs and a worker revision census are private under
`~/.cache/tidepool/shoal/runs/b1b6bd40-db07-4862-9f73-34bcd77af7fd/wrapup/`.
Process shutdown/restart requires a separate explicit user decision going forward.

## Current implementation decision

[NEXT.md](../../../NEXT.md) is the active handoff. One Astra Medium implements
linearly, without delegation. Preserve existing interactive Codex TUIs as the
human steering/review interface; do not adopt the former controller/observer
migration or invent a new operator route.

TOML remains core configuration. Project Haskell modules and Markdown prompts
are ordinary repository source selected at startup and fixed for the swarm,
including later actors. Changes activate at an explicit swarm boundary.
Sol pilots Markdown plans through reusable Haskell primitives and project
recipes. The human starts an ordinary Astra session for RSI. Keep the requested
typed observations and existing steering primitives; no dedicated RSI lifecycle,
budget governor, specialist-admission controller, or service gate is required.

The existing `72d1628c…` TUI fallback fix is relevant to this path. Verify the
selected binary and delayed-completion responsiveness at the owning code boundary;
do not mistake this historical evidence for current build/deployment acceptance.
