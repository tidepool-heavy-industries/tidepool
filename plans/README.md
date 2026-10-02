# Plans and retained evidence

Source and owning crate guides define current behavior. This index points to
unfinished acceptance, live follow-up, and evidence still named by source or
tests. Historical proposals and completed investigations are kept in Git only.

## Active acceptance and delivery

- [Final engine and embedded harness delivery](engine-harness-final-delivery.md):
  current M1/M2, compiler/runtime, actor, harness, and release acceptance ledger.
  Its source checkpoint explicitly says neither M1 nor M2 is accepted; do not
  treat individual candidate checks as joined acceptance.
- [Bounded model turns](bounded-model-turns.md): caller-owned tools, budgets,
  remote build qualification, and run timeline evidence.
- [ModelCall integration handoff](model-call-integration-handoff.md): admitted
  execution hook and joined resident acceptance.
- [Workbench acceptance](workbench-acceptance.md): remaining resident validation,
  lifetime cleanup, adapter gates, and live evaluation.
- [Release preparation handoff](release-preparation-handoff.md): immutable
  package selection, producer proof, retention, and state compatibility.

## Live follow-ups

- [JIT memory lifetime](actor-model/jit-memory-lifetime.md): live-machine code
  reclamation remains open; actor retirement alone does not establish code is
  unreachable. The actor guide links to this contract.
- [Retained review and runtime failure observations](next-wave-inputs.md):
  source-linked wave-3 context and unresolved retained-review, pending-call, and
  queued-checkpoint investigations.
- [Shared-identity audit](p7-shared-identity-audit.md): historical parcel-7
  finding whose current status needs fresh acceptance evidence; it is not proof
  that M2 is complete or that the old defect still reproduces on current source.

## Regression evidence and support

- [Observation-budget finding](jev-lab/observation-limit/FINDING.md): fixed
  behavior retained by `bridge/facade/src/actor_host/observation_budget_tests.rs`.
- [`next/WaveContract.hs`](next/WaveContract.hs) is imported by evidence actors
  and their tests; it is test support, not a design proposal.
