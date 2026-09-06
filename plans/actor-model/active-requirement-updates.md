# Active requirement updates

Proposed next slice after quiet observations. This is a contract to implement,
not a claim that current requests steer active work. The shoal-repl interviews
are qualitative accounts from an end-user model without runtime source access;
they distinguish observations, interpretation, and untested proposals.

## Acceptance scenario

A coordinator has an outstanding tabs assignment. Its parent clarifies that
tabs must be clickable while implementation is underway. Today a second
`request` queues a separate assignment; the reported run delivered keyboard-only
tabs first and mouse support through the follow-up. The new operation should
present the clarification during the original assignment, preserving its
response handle and pending obligation.

## Runtime contract

- Give an update its own identity and target the exact active assignment.
  Keep queued new assignments and active updates distinct in types and tools.
- Present an update at the next safe model boundary. During a tool call, finish
  the call first and preserve its committed effects. While awaiting a specialist,
  wake the coordinator with its original assignment still pending.
- Report queued-for-presentation, presented, and too-late outcomes explicitly.
  A completed or superseded assignment must not silently receive an update as
  unrelated new work. Fence presentation against assignment completion races.
- Delivery acknowledgement means presented, not understood or incorporated.
  The actor can separately report its intended response. Task-specific typed
  evidence can establish later incorporation: a revised contract, candidate
  commit, and checks for coding work; other tasks choose other evidence.
- Keep published candidate, parent acceptance/integration, recipient presentation,
  and recipient incorporation separately observable. None implies the next.

Actor request lifecycle and presentation sequencing belong in `tidepool-actor`.
Provider delivery/configuration mechanics belong in `tidepool-agent` and its
backend. Verify each backend's safe input boundary before exposing capability.
`withEffort` already configures inherited context forks; Codex configuration
updates are a separate backend mechanism. Do not infer a missing backend
capability from the absence of a Shoal operation for an active assignment.

Exercise reasoning, an in-flight tool call, waiting on a specialist, and a race
with final delivery. Verify original response identity, exactly-once presentation,
preserved effects, and explicit too-late reporting. Scaffold validation remains
proportional to the obligation: compile representative consumers when that proves
usability, while allowing deliberate partial scaffolds.

Quiet observation is independently useful and must remain independently shippable.
Observation-aware suppression of delayed notices is a later notification-owner
change; retained results must remain repeatably inspectable.
