# Managed coding checkouts and repository observation

This crate owns checkout creation/retention, its durable registry, repository
inspection, coalesced events, snapshots, and the narrow typed merge primitive.
`Worktree` remains the workflow term. Managed checkouts are native linked Git
worktrees: working files, index, and HEAD are per actor; objects, refs, config,
and administrative metadata share the source repository's namespace.

- Never dirty the source working tree. Registry state, managed repositories,
  journals, and temporary indexes live outside it.
- Shared Git metadata is intentional collaboration infrastructure, not an
  isolation boundary. Do not add publication/import machinery between actors.
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
- Do not add general workflow verbs. `try_merge` is the one typed
  merge/abort boundary; conflict resolution stays authored policy.
- Test git behavior against real temporary repositories, never a mocked git.
  This crate is GHC-free and suitable for focused ordinary Cargo tests.
