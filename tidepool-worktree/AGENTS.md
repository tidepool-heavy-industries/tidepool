# Managed worktrees and repository observation

This crate owns worktree creation/retention, its durable registry, repository
inspection, coalesced events, snapshots, and the narrow typed merge primitive.

- Never dirty the source working tree. Registry state, managed worktrees,
  journals, and temporary indexes live outside it.
- Retain first: no deletion, GC, or silent recreation of a missing managed
  worktree without an explicit design decision.
- Every git subprocess goes through `GitCli` so environment scrubbing and
  failure receipts cannot drift.
- Reconciled repository inspection is authoritative. Hooks and filesystem
  events may wake polling but never supply facts.
- Observations are coalesced state deltas, not causal histories. Degrade to
  `UnknownChange` rather than inventing attribution.
- Subscriptions start at the journal's current end. The durable journal is for
  traceability and diagnosis, not handler replay.
- Do not add general workflow verbs. `merge_branch_into` is the one typed
  merge/abort boundary; conflict resolution stays authored policy.
- Test git behavior against real temporary repositories, never a mocked git.
  This crate is GHC-free and suitable for focused ordinary Cargo tests.
