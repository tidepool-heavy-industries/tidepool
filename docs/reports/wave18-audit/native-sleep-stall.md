# Native sleep blocked queued steering — 2026-09-27 UTC

Wave18 was active, not accepted or abandoned. Root explicitly reported no
acceptance and pending producer/browser work. Operator actor11/request7 was
coordination-stalled: its native `sleep` call at03:41:02.708 requested43200000ms
and did not return until04:31:28.691. The retained result reports3025.9724s and
“Sleep interrupted by new input.” Its rollout is
`/home/inanna/.codex/sessions/2026/09/26/rollout-2026-09-26T16-47-43-01a0e01e-1582-70d0-bc0c-d9be18f34d72.jsonl`.

Root's04:29:03 report identified request7 open, no replacement test worker,
notification64 submitted but unpresented, and assignment update2 pending.
Producer exact review had separately settled; its owner still owed typed delivery.
These are separate coordination edges, not evidence that product acceptance passed.

## Mechanism and intervention

Pinned Codex input_control admits host StartOrSteer into QueuedItemService.
Its wake_if_loaded emits an idle lifecycle only when idle; on_thread_idle
owns dispatch_if_idle. Native sleep holds the active turn and wakes on actual
core InputQueueActivity::Steer. Consequently host notification admission alone
cannot wake this active sleep. An interrupt alone is also insufficient evidence:
queue on_thread_idle skips Interrupted and explicit interruption owns its wake.
Sources: vendor/codex/codex-rs/tui/src/host_dynamic_tools/input_control.rs; queue service and core
sleep/turn_input implementations in the pinned client. Verify these owners before
changing dispatch policy: active steering has broader ordering/authority semantics.

The supervisor sent one direct TUI user steering message to the identified
actor11 pane%598. After submission, sleep returned and the actor executed status
and lookup again. No actor retirement, process restart, shared-daemon restart or
worktree mutation was used. Root was notified of the successful wake. Resumed
tool calls prove wake, not incorporation of update2 or completion of browser work.

## Scoped mitigation and follow-up

Teach all roles to end the model turn normally when awaiting Exomonad events;
do not park in native sleep. The pending prompt patch adds this at the existing
wait/continuation guidance. Preserve the runtime defect separately: decide whether
native sleep should be unavailable in hosted Exomonad, or whether explicit host
steering should wake active turns under a supported contract. Do not quietly
convert all queued notifications into interrupts. Retain a regression for queued
steering during native sleep in the eventual owning fix.
