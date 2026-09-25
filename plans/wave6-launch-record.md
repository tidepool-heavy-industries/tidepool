# Wave 6 launch record (2026-09-25 04:55Z)

| Item | Value |
|---|---|
| Run id | 02a1c2fd-cd36-4c88-8d4f-4999edcca7a0 |
| tmux session | wave6 (root in window 3) |
| Deployed binary revision | tidepool main 4f036d68d, redeployed 04:51Z |
| Workspace pin | 46de15cfe528de3e70ff7d65e1cba7654ecafd75 (integrate-ws2) |
| Harness master | d0245b3 (wave6-prompts merged over the operator's e44033d) |
| Brief | "Read NEXT.md in the repository root and follow it." |
| Log path | .exomonad/logs/02a1c2fd-cd36-4c88-8d4f-4999edcca7a0.log |
| Run dir | ~/.cache/tidepool/exomonad/runs/02a1c2fd-cd36-4c88-8d4f-4999edcca7a0 |

Launched co-resident (fresh machines still disabled) from the deployed
build carrying: delivery fence fix (defer during computing cells,
withdraw-and-resubmit, per-child delivery status line), single split
attempt on every split path, memo completion of unused modules, reply
refusals as request state, unique Project constructor names, watchdog
tool-first, discard hold, run-map review sections, review skill text.
Gate: `just verify` skipped by operator decision (optimistic launch);
redeploy's own build passed. Shared compile daemon restarted once after
the extractor change (pid 3604812).

Trials (NEXT.md): escalate after two failed check rounds; expected-red named;
fence handoff within one turn once the fence outlasts a checkpoint; every
lead sends the admission checkpoint unprompted.

Observe with `exomonad run-map <run-dir> --since 30m` (deliveries,
notifications, slowest calls, rejections and nudges, cancellations).
