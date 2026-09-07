# Wave closeout and comparison with NEXT.md

Source checkpoint: Tidepool main `5ba83eb2bef9ba32d9303b08d7d7a779e2de0323`.
The original NEXT.md plan is preserved below its status notice; its claims that
implementation has not started and native controller ownership is missing are stale.
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

The central product outcome remains incomplete despite substantial prerequisite
work. There is no production RemoteAppServerClient/controller consumer in the
Tidepool adapter. See [the exact native client handoff](../service/native-client-handoff.md)
and [the acceptance matrix](../acceptance.md); these are the useful next inputs,
not the old worker dispatch instructions.

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

The separate Codex repository contains committed fallback fix `72d1628c76` on
controlled-execution-observer: 60-second settlement attempts and disabling hosted
tools instead of fatal TUI exit. Tidepool still pins `600be9df`, so the fallback
fix is not in its pinned binary. These closeout changes have not been pushed.

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

Next work should compile a real native controller client against the chosen exact
pin, establish one no-TUI hosted call and durable completion, then attach the
observer. Finish intent routing and the mounted failure/retirement matrix before
opening small-worker scope. Publish/build/pin the separate fallback fix deliberately;
do not assume the existing native pin includes it. Run-map usage wiring can follow
the existing exported reader without another parser or provider-home scanner.
