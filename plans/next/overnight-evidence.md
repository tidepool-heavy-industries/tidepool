# Overnight numeric run — inspected evidence index

This is a bounded retrospective handoff, not a raw transcript or new durable log.
Observed 2026-09-06 from actual run files, provider records and tmux, plus attributed
source investigations. Local private paths may disappear after restart/rotation.
Do not commit full provider captures. Future tooling should reproduce and qualify
these findings, not assume retained terminal history was complete.

## Source and artifacts

- Numeric accepted/tested source: `95e5eed6cd04f850d135b5d0ba640767310d52c4`.
- Outcome/docs `1746e7153a3620c22c03cece945aedd000810fd4`;
  retrospective baseline `e5a1842dd4d3302d6eb8c0519a678937599d9c7e`.
- Product details: `FLOATING_POINT_BUG_REPORT.md`. Detailed typed survey conclusions
  were folded into `plans/actor-model/live-context-unfold-dogfood-followups.md`
  (numeric sections around line 2125 in inspected revision). Read on demand.
- Run ID `01cb07e8-53a9-4be7-a78f-c28a1426035a`; directory
  `/home/inanna/.cache/tidepool/shoal/runs/01cb07e8-53a9-4be7-a78f-c28a1426035a`.
- `status.json`: project `/home/inanna/dev/tidepool`, tmux `shoal-tidepool`,
  root provider thread `01a07607-8f8b-7423-bf03-9d10a7e0b243`.
- `<actor>-1/binding.json`: provider thread; inbox.jsonl and cursors: sequenced
  activations including sessionReady/request/input metadata. root-binding.json,
  host-incarnation.json (1) also present. Failed actor9 had no provider binding.
- Provider rollout files: `/home/inanna/.codex/sessions/2026/09/06/rollout-*<thread>.jsonl`.
- `tmux capture-pane -p -J -t shoal-tidepool:2.1 -S -` retained 219 host log lines
  at inspection. Pane addresses are historical, not stable handles. Separate
  `shoal-shoal-repl` session is unrelated; do not mix its records.

## Observed map (approximate times, UTC)

| Time | Work/evidence |
|---|---|
| 09:23–09:30 | root scaffold; technical leads' initial responses |
| 09:32 | independent literal ingress branch |
| 09:38–09:46 | first review waves |
| ~09:56 | finite-show diagnosis actors21/22 |
| ~10:01 | primitive-demand repair actor23 |
| ~10:10 | deeper demand review/subnormal work actors26/27 |
| ~10:12 | native decode oracle actor28 |
| ~10:16 | final decode reviewer actor29 |
| ~10:24–10:26 | numeric completion, independent hosted acceptance, root fold |

Historical numeric roster was root + actors1–29, including failed9, not 30 concurrent
workers. Later investigator30 is retrospective, excluded from numeric usage totals.
Technical group1 had 26 descendants, ingress group6 had 3. Review found constant-folded
probes, cleanup holes, unsafe eagerness and subnormal normalization issues; the tree's
value was additional findings and repairs, not demonstrated speed/cost superiority.

## Usage measurement actually performed

Read `token_usage_record.payload.usage`; filter payload.thread_id to binding thread,
dedupe response_id; numeric actors0–29 only; timestamp before
`2026-09-06T10:27:00Z` to exclude later retrospective work. Do NOT sum cumulative
`token_count` records: inherited fork histories contain earlier totals.

| Recorded measure | Value |
|---|---:|
| Unique response records | 905 |
| Input tokens | 88,699,459 |
| Cached input tokens | 86,888,192 |
| Cached/input ratio | 97.95797% |
| Output tokens | 165,462 |
| Reasoning tokens (subset of output) | 32,967 |
| Input + output | 88,864,921 |

These are locally recorded usage within that selection. They are not pricing,
normalized provider-prefix equality, peak concurrency or a serial-vs-tree benchmark.
No controlled cost/latency comparison was run. Reader tooling should detect duplicate
conflicts and report missing files rather than silently reproduce these totals.

## Failure evidence and limits

Retained host history had 11 `request update not presented` initialize-closed warnings
and one WorktreeUnauthorized. Log ordering showed the denial before actor9 resource
allocation log; no provider launch/binding for9. Source reveals early worktreeHead
before asynchronous host binding. Specific interleaving/principal snapshot was not
recorded, so this is a supported readiness-gap hypothesis, not a proven exact race.
The same boundHead seed admitted sibling10 successfully; replacement13 succeeding
with a new allocation does not diagnose the cause.

Steering source investigation inspected pinned native revision
`c8460ffd7c859da2a1467f4384020cf9a19bcc69`: raw-copy proxy versus WebSocket upgrade,
and global daemon assumption versus `--destination-local` embedded server. Current
default socket was absent; historical socket presence is unknown. This broken route
was for updateRequest; ordinary typed messaging used another route and worked.
No historical per-proxy stderr artifact pinpointed every warning. Shell JSONL tests
bypassed actual transport/topology. New service/controller design was source-reviewed,
not run as a canary during planning.

## Cleanup observed

After investigation the root inspected and executed cleanup groups1,6,24. All three
receipts complete; 30 selected old actors retired (failed9 already stopped). No Git
worktree, branch, commit or user-file deletion. Root stays attached for user restart.
Cleanup retired request/watch metadata: old handles are not a handoff mechanism.
A subsequent live roster contained only root with no current requests.
The receipt is lifecycle evidence, not an independent process leak audit.
