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
- [Jev lab results](jev-lab/RESULTS.md): retained temporarily because the
  recipe-check lane uses its failure shapes as fixture candidates. Remove it
  after those behaviors are represented and verified in the fixtures.
- [Jev breadth survey](jev-lab/breadth/RECOGNIZING-FIT.md) and
  [unrun ideas](jev-lab/breadth/NOT-RUN.md): retained while `NEXT.md` links to
  them. The survey is dated 2026-09-17 and is not current implementation
  evidence.

## Test support

[`next/WaveContract.hs`](next/WaveContract.hs) is imported by the evidence
actors and their tests; it is source support, not a design proposal.
