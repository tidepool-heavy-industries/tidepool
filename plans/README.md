# Plans and retained evidence

Source, tests, and the owning crate guides define the current system. This
directory keeps only open design questions, evidence still named by a test or
guide, and Haskell support imported by its consumers. Completed designs and
unconsumed proposals are removed; Git retains their history.

## Open design questions

- [Workbench acceptance](workbench-acceptance.md): async workbench validation,
  lifetime cleanup, pending-cell adapter gates and separate live evaluation.

- [Final engine and harness delivery](engine-harness-final-delivery.md): accepted
  implementation wave through M1/M2, recovery, structural performance and native
  Buck release readiness; live trials and publication remain separate.

- [Engine and harness implementation](engine-harness-integration.md): approved
  parallel foundation closure, compiler/native and Git/process redesign, shared
  private notebook execution and embedded harness; stop at verified readiness.
- [Module product review handoff](module-product-review-handoff.md): exact
  producer proof, checks and decisions for the next compiler/native review.

- [Engine foundation](engine-foundation.md): approved compiler context/artifact,
  native code lifetime and external execution redesign after the wave22
  investigation; implementation ledger and acceptance evidence.

- [Dedicated swarm host](hetzner-swarm-host.md): 128 GB Hetzner host, declarative
  NixOS installation, resource budgets, recovery and staged wave migration.

- [Structural performance ledger](structural-performance-a.md): matched
  measurements are complete; the remaining rows are questions, not scheduled
  implementation work.
- [JIT memory lifetime](actor-model/jit-memory-lifetime.md): live-machine code
  reclamation remains an open follow-up referenced by the actor guide. No
  reclamation design is accepted.
- [Embedded harness integration](harness-integration.md): accepted host/library
  contracts, Codex compatibility, implementation parcels and acceptance gates.
  Compiler/runtime implementation is held pending the engine investigation.
- [First embedded Haskell worker tree](harness-first-tree-prd.md): handoff to
  the engine owner, remaining resident integration, and connected release gates.
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

- [Installed-request transfer review](foundation-installed-tools-transfer.md):
  short admission lifetime, request-owner gaps, and source/handler publication
  semantics for the staged host adapter.
- [Retained-view launch measurement](foundation-view-spawn-measurement.md):
  bounded small/large synthetic-host comparison and spawn syscall trace for
  the thin namespace command helper.
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
