# Wave20 readiness

Updated 2026-09-28 UTC. Launch is authorized after the remaining gates.
The retry launched successfully; root queue-ready handshake completed.
No recurring timer is active.

## Integrated and checked

- Git-aware source import and shared copy admission: `6360d2329`,
  `476f7228e`, `4c46712e8`. Concurrent admission and process-death lock release
  each passed a focused test.
- Retired checkout layers, recovery and last-reference release are integrated.
  The production four-child fork/retire test passed; the separate 16-manifest
  unit test is not a production storage measurement. Durable state now lives
  outside the disposable cache (`44f418d98`); legacy discovery is fenced.
- Context checkpoints, explicit release and fallible record sends are
  integrated. Six combined facade checks passed at `44f418d98` (535 skipped,
  52.088 seconds), including the hosted deferred-checkpoint and mailbox cases.
  Checkpoint sponsor accounting was subsequently repaired at `65cfd1408`;
  sponsor-budget and configured-spec checks passed, two tests total.
- Coherent run tooling and historical product-source isolation are integrated
  through `72efbbca4`; six focused source/recovery checks passed. Catalog v44
  and fingerprint were fixed at `ff5c3babe`, with its exact test passing.
- The local check wrapper preserves validated paired extractor overrides and
  starts/reuses its compiler daemon (`840d933e8`; 28 shell checks passed).
  The earlier direct-binary workaround started a cold GHC worker per request.
  The corrected path reuses worker 0: one observed cold request was 56.935s,
  followed by requests of 2.253s, 7.035s, 0.973s and 12.514s. These are individual
  compiler requests, not full recipe durations or a uniform latency promise.

## Final integration and launch

The helper package is pinned at `b270dbf86b5c755780ca1c04805fa786153b578c`
(shared origin main), integrated into Tidepool at `2b3360254`, and pinned into
wave20 at `6af0f58`. Scaffold and shipped template match.

- Hosted happy/correction checks passed 13 assertions; separate final targeted
  regressions passed 13 assertions for repeated review, parallel exact-source
  verification, missing incorporation refusal and cleanup. These were split
  focused runs, not one complete aggregate run.
- Pin and prompt-catalog checks passed 2 tests (539 skipped).
- The actual launch workspace passed source and child-effect preflight;
  definition identity is
  `f86c3c075f679bcb7af145f37418757c42e63c783d8b95bccbd2d758b997cd7d`.
- First run `e467e715-35af-40af-8c98-14be65564eb7` failed before a model
  conversation: its durable operator socket path exceeded Linux SUN_LEN.
  Run resources were stopped with the owning CLI; service inactivity and absent
  wave20 tmux session were verified. Logs remain in the launch checkout.
  No root interview was possible before conversation creation.
- `9bd6b3df1` fixes the owning operator transport using a retained directory FD
  and short proc-fd address, preserving the actual durable socket location.
  Long-path HTTP/permissions/cleanup and artifact-query tests passed (2 tests,
  540 skipped). The retry used the rebuilt binary successfully.

Retry run `4d826f44-f964-40e1-82d2-a74ebf405222` started at
2026-09-28 01:40:26 UTC. Root `1@1` completed its queue-ready handshake at
01:41:44 UTC on conversation `01a0e5ac-d39f-7d70-a50c-e8a7ceed26ba`.
The operator socket is ready. Exact hashes and launch evidence are retained in
`/home/inanna/dev/exomonad-harness-runs/wave20/docs/wave20-launch.md`.

Follow-up: the first failure also exposed a pre-conversation recovery mismatch:
recovery says it can start fresh but subsequently refuses incomplete root
conversation evidence. The stopped failed run's logs retain this secondary
failure; it is not evidence that a provider session ran.

The coordinator exposes exact review-flow handles for the original owner to
clean up. It cannot impersonate that owner. Terminal plan completion and that
explicit external cleanup step remain distinct.

## Product preparation

`/home/inanna/dev/exomonad-harness-runs/wave20`, branch `rsi/wave20`, starts from
wave19 checkpoint `d22c5a525adb104eedece0fb9fce373520c8b7ad`. Its brief and NEXT
are committed (`f7e487c`, `c553a6b`). See
[wave20 preparation](../docs/reports/wave20-preparation.md).

Sol Medium first reconciles surviving wave19 candidates and freezes the shared
contract, then coordinates three parallel Luna component trees: deterministic
asynchronous custom cells, typed job-side agent operations, and reusable tree
lifecycle. These use standalone stubs; production Exomonad integration and
credentialed inference remain outside this assignment. One bounded real
WorkPlan episode measures coordination value with ordinary orchestration as a
fallback.

Wave19 root and worker panes are dead; its compiler remains alive. The root
interview is unavailable after the crash. Dirty wave19 NEXT, trial notes,
friction notes and surviving commits are preserved. Do not restart shared
daemons, discard work, or treat this checkpoint as an accepted integrated release.

## Compiler recycling observation

The matching persistent daemon started at 2026-09-28 01:11:14 UTC with one
worker and a 7168 MiB RSS ceiling. It recycled workers at 01:13:34 (7478 MiB,
8 requests) and 01:16:54 (7376 MiB, 18 requests). Request
`d9aba353205d11ad` then took 52.760 seconds: its 139-module compilation reported
51.211 seconds wall time, including 26.675 seconds lowering, 13.426 seconds
interfaces and 5.320 seconds typechecking. Evidence is in
`~/.cache/tidepool/battery-daemon/compiler.log` and the retained test artifacts.

Daemon reuse fixes the accidental fresh-worker-per-call path; it does not
remove memory-triggered recycling. Follow up on retained compiler state and
unnecessary module loading after launch. Do not infer a uniform notebook
latency from the observed 0.3–4-second warm requests, or raise the memory limit
without considering simultaneous run/compiler memory.
