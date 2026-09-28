# Plans and retained evidence

Source, tests, and the owning crate guides define the current system. This
directory keeps only open design questions, evidence still named by a test or
guide, and Haskell support imported by its consumers. Completed designs and
unconsumed proposals are removed; Git retains their history.

## Open design questions

- [Bounded continuations](bounded-continuations.md): dependency routing, retained
  failure investigation and checked repair, with next-wave adoption and evaluation.

- [Dedicated swarm host](hetzner-swarm-host.md): 128 GB Hetzner host, declarative
  NixOS installation, resource budgets, recovery and staged wave migration.

- [Recursive delegation](recursive-delegation.md): canonical live scaffold/unfold/
  integration workflow, Luna implementation trees and reusable event routing.
- [Wave19 coordination rethink](wave19-coordination-rethink.md): retained evidence
  for the superseded WorkPlan design and prerequisite ownership failures.
- [Jev pattern building blocks](jev-pattern-legos.md): opinionated composable
  judgment patterns and decision trees; proposed interfaces and evaluation plan.
- [Haskell authoring improvements](haskell-authoring-improvements.md): bounded
  diagnostics/assertion work and the intermediate Jev layer.

- [Post-wave17 improvements](post-wave17-improvements.md): planned worktree batch
  for helper/mount integrity, recoverable checks, coordinator cleanup, executable
  release evidence and measured coordination/launch costs.

- [Post-wave15 improvements](post-wave15-improvements.md): completion-oriented
  Bash with bounded contextual follow-ups, evidence access, source composition
  and review friction; next RSI experiment and measurements.

- [RSI iteration 3](rsi-iteration-3.md): wave-10 interview decisions, stale
  reminders, incremental typed review tools and proposed offline restart wave.
- [Request scoped typed tools](request-scoped-tools.md): design check for
  current typed request access and a review acceptance tool.
- [RSI iteration 2](rsi-iteration-2.md): wave-10 preparation and observations,
  launch repairs, review-cell evidence and deferred indentation assistance.
- [RSI iteration 1](rsi-iteration-1.md): wave-8 reconciliation, typed review
  provenance, programmable coordination, and the next harness wave's evidence.

- [Inherited-resource permissions](inherited-resource-permissions.md): active
  implementation of shared observation with owner-controlled mutation, including
  listener/release races and caller-relative checkout seeds.

- [Structural performance ledger](structural-performance-a.md): matched
  measurements are complete; the remaining rows are questions, not scheduled
  implementation work.
- [JIT memory lifetime](actor-model/jit-memory-lifetime.md): live-machine code
  reclamation remains an open follow-up referenced by the actor guide. No
  reclamation design is accepted.
- [Harness adoption](harness-adoption.md): how Exomonad moves from the forked
  Codex backend to the standalone model harness (`~/dev/exomonad-harness`):
  adapter, `spawnAgent`, hook placement, compaction, deletions, order.
- [Harness adoption reconciliation](harness-adoption-reconciliation.md): current
  source audit and experimental gates for a real resident cell and small worker
  tree; production migration remains a later decision.
- [Dogfooding sweep](dogfood-sweep.md): the accepted decisions and open
  checklist from the 2026-09-23 run audit (latency attribution, run-map
  provenance, delivery and input-acknowledgement friction).
- [Next-wave inputs](next-wave-inputs.md): structural follow-ups found in the
  2026-09-23 review wave, each with its evidence and bug class.

## Referenced evidence

- [Observation-budget finding](jev-lab/observation-limit/FINDING.md): the
  regression invariant is covered by
  `bridge/facade/src/actor_host/observation_budget_tests.rs`.
- [Compile-memo evidence](compile-memo-evidence.md): why a parent/child
  checkout pair misses the compile memo on byte-identical dependencies
  (`HomeDependencyWitness`'s digest mixes in selected path, not just
  content fingerprint) and the `TIDEPOOL_MEMO_TRACE=1` diagnostic added to
  confirm it directly. Covered by
  `tidepool/extract-cmd/src/diagnostics.rs`'s
  `machine_stderr_prefixes_match_the_haskell_emitters` and
  `tidepool/runtime/src/span_blocking.rs`'s tests.
## Test support

[`next/WaveContract.hs`](next/WaveContract.hs) is imported by the evidence
actors and their tests; it is source support, not a design proposal.
