# Plans and retained evidence

Source, tests, and the owning crate guides define the current system. This
directory keeps only open design questions, evidence still named by a test or
guide, and Haskell support imported by its consumers. Completed designs and
unconsumed proposals are removed; Git retains their history.

## Open design questions

- [Structural performance ledger](structural-performance-a.md): matched
  measurements are complete; the remaining rows are questions, not scheduled
  implementation work.
- [JIT memory lifetime](actor-model/jit-memory-lifetime.md): live-machine code
  reclamation remains an open follow-up referenced by the actor guide. No
  reclamation design is accepted.

## Referenced evidence

- [Observation-budget finding](jev-lab/observation-limit/FINDING.md): the
  regression invariant is covered by
  `bridge/facade/src/actor_host/observation_budget_tests.rs`.
## Test support

[`next/WaveContract.hs`](next/WaveContract.hs) is imported by the evidence
actors and their tests; it is source support, not a design proposal.
