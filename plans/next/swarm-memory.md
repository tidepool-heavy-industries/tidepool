# Shared swarm memory containment

One configured user `swarm.slice` contains every Shoal run: reclaim at 16 GiB
RAM, hard RAM limit 18 GiB, swap limit 24 GiB. Nix stays separate at 8 GiB RAM /
1 GiB swap, one build job and compilation-job default, and a two-CPU quota.
The host has 40 GiB swap and swappiness 60. NixOS activation and live limits have
been verified. There is no 20-second pressure-kill policy. Limits are ceilings,
not reservations; kernel OOM remains the last resort at aggregate exhaustion.

## Runtime contract

`[launch].systemd_slice` defaults to `swarm.slice` and freezes with the workspace.
Bootstrap, compiler, host, each native supervisor/TUI and the shared command
resource service explicitly enter it. An existing outside tmux server supplies
no containment inheritance. `in-slice` checks actual membership before executing
a payload; systemd environment expansion is disabled to preserve literal argv.
The selected slice must have finite memory/high/swap limits. A misplaced shared
resource service is rejected without replacing it or retrying retained jobs.
Run diagnostics record effective limits in `resource-budget.json`.

Existing command owners, per-command budgets, process supervision and namespace
entry remain responsible for their own resources. No additional scheduling or
Haskell control protocol is introduced.

## Release validation

Focused checks completed before the final release build:

- Three `tidepool-node::systemd_slice` checks: finite configuration, membership,
  and literal launch arguments.
- Three actual `shoal_namespace_entry` checks: retained namespace entry,
  rejection before an uncontained payload, and two independent scoped windows
  launched through an outside tmux server.
- Seventeen Shoal configuration/launch unit tests.
- `full_tui_survives_command_oom_and_accepts_steering`: actual matched native TUI
  and local scripted provider, passed in 413 seconds with scoped supervision.
- Two actual `shoal init` runs: private offline Codex homes, compiler/host/
  supervisor placement, effective limits, and the same shared resource service.
  Evidence: `target/slice-launch-acceptance-20260910-r2` in the canonical checkout.
- A 64 MiB child service with 32 MiB swap reached kernel OOM; the outside
  control survived. Its exact systemd result and limits are retained alongside
  the bootstrap evidence.

`scripts/tests/shoal_launch_acceptance.py` reproduces the offline bootstrap check
with explicitly selected binaries and retains diagnostics. Its additional small
budget probe checks child OOM with a finite swap allowance and an outside control.
Run it only at an idle shared-service boundary. It cleans up only its exact test
sessions/scopes and service; source and logs remain available.

Focused package checks passed: `Project.Checks.context` (one assertion) and
`Project.RoutingChecks.twoLaneHandoff` (eight assertions). The broad recipe run
was deliberately stopped; it is not claimed as passed. Clean-environment packaged
bootstrap passed: two Ready TUIs, wrong-slice service rejection without disturbing
the original service, and finite child OOM. Evidence:
`target/slice-launch-final-20260910`. The final selected workspace also compiles. Codex `7259e937` and the extractor are built and reusable.
Record their actual identities in the next launch selection; do not infer them
from a product branch's checkout or old launch record.

## Next run

The operator confirmed the account switch and authorized the next launch on
2026-09-10. All Sol execution helpers and project guidance select
Medium, including bounded workers and review. The next wave resumes the R7
applications wrap-up and remaining engine implementation through the current
[launch guidance](../parallel-dogfood/launch.md).

The previous run's 32 dead panes and Git state remain preserved for handoff.
`target/dogfood-retirement-20260910` contains archived scrollbacks and process/run
maps. Nothing in this change restores its live requests or Haskell handles.
