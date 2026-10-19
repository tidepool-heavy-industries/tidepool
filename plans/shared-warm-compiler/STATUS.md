# Campaign status and restart entry point

State: **plan written and cross-reviewed; ready for the user-started overnight run**.
No compiler implementation, build, service change or deployment has been started
by this research fork.

Plan base: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`.
Plan branch: `plan/shared-warm-compiler-20261019`.
Implementation base: **latest coordinated release tip at execution kickoff**;
refresh and record it, including required peer correctness fixes.
Campaign integration branch: **not yet created**.

Execution policy: Sol-low root with the user's goal loop, Sol-high campaign
supervisor, Sol medium/high workers/reviewers, and optional Luna rote helpers.
On a genuinely hard strategy question, finish all already approved unblocked
work and its integration, then terminate the loop with a morning decision packet.
See [OVERNIGHT.md](OVERNIGHT.md).

## Plan ownership

| Area | Researcher writing the initial plan | State |
| --- | --- | --- |
| Root architecture and shared contracts | Root | Written; cross-area findings resolved |
| Supervision and campaign kickoff | `research_incremental_options` | Written and cross-reviewed, including final overnight policy |
| Host input roots and admission | `research_incremental_validity` | Written and cross-reviewed |
| Owned inputs and byte lifetime | `research_structural_sharing` | Written and cross-reviewed |
| Warm compiler and frontend experiment | `research_ghci_sessions` | Written and cross-reviewed; unnecessary budget feature deferred |
| Reload retirement | `research_query_lifecycle` | Written and cross-reviewed |
| Qualification and metrics | `research_hls_incrementality` | Written and cross-reviewed |

After initial authorship all six researchers reviewed the combined tree and
neighbouring production seams. Findings and dispositions are recorded in
[REVIEWS.md](REVIEWS.md). This is review of the plan, not qualification of code.

## Implementation progress

All implementation leaves are **not started**. The first executing supervisor
replaces this section with a compact table containing, per active or completed
leaf:

| Required field | Purpose |
| --- | --- |
| Area/leaf and immediate supervisor | Actual ownership and reporting route |
| Agent canonical name and state | Running, waiting for a named input, reviewing, completed or stopped |
| Contract revision and base SHA | Which decisions/source the worker consumed |
| Worktree, branch and candidate SHA | Recoverable concrete output |
| Prerequisite SHAs | Exact dependency relation |
| Next action and owner | Prevent silent waiting on review/merge |
| Evidence directory, command, selected/executed counts and result | Separate compile, behavior and measurement |
| Review/fold commit or explicit disposition | No orphan candidates |

Each area retains a short handoff with its own child waves; root status links to
it instead of duplicating every log. Messages are notifications, not the only
record. Active worker slots and active heavy build lanes are counted separately.

## Known integration obligation

The peer's selected-root defect repair is in the plan base. The stronger hosted
Persona case was pending at handoff. At kickoff inspect the retained outcome in
[EVIDENCE.md](EVIDENCE.md), adopt any later parent fix, and record the actual
agreed base. Do not silently relabel pending validation as passed.

## Restart procedure

1. Inspect Git and live agents; do not assume a transcript completion means work
   is merged or a process has settled.
2. Check active builds/services and resource headroom before any new admissions.
3. Read the shared contracts and each area's latest handoff/candidate.
4. Reconcile the table against actual branches and exact review evidence.
5. Resume or replace only the workers with a concrete dependency-ready task.
6. Reconfirm integration ownership if the parent release branch moved.

No cleanup, output retirement or cache purge is part of this planning task.
