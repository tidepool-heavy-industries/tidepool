# C — End preparation before polling publication

Own actor initialization in exomonad/actor/src/resident_actor.rs. Extend into the
named generation transaction in resident_workbench.rs only if the diagnostic
below demonstrates that need; coordinate D's test-only edits separately.

## Diagnosis and evidence boundary

The original fault was a finite async poll chain, not recursive Haskell or an
unbounded Rust recursion. Nine simultaneous native poll reservations totaled
1,970,272 bytes before the libtest wrapper and Tokio/task-local frames. Boxing
stabilization and publication reduced that measured prefix to 1,715,760 bytes,
but actual compile55 Discovery still overflows. Its trace now reaches a third
checkout, beyond the old fault; its exact new fault suffix is not captured.

The structural problem is retaining large preparation frames while later phases
execute. A BoxFuture handle is small, but its poll and construction frames can
still be large. Do not claim heap allocation of a future alone solves stack use.
Do not add another forwarding-helper box solely from the old prefix sum.

Detailed retained handoff, executable and working diagnostic recipe:
/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/introspection-stack53/deepseek-stack-handoff.md.
The two boxing commits d6826c9663 and 220ad277a5 are already integrated; do not
reapply them or call their source review a default-stack pass.

## First discriminating observation

Use one first-fault GDB replay of the exact retained compile55 host executable
and its declared owned-service resources, with RUST_MIN_STACK unset. Follow the
retained recipe through the actual isolated runner/owned-daemon wrapper, retaining
its target environment, especially TIDEPOOL_GHC_LIBDIR. Write new evidence; do not
overwrite older traces. Capture first SIGSEGV, PCs/SPs/FPs, mappings and the full
active chain before continuing to the normal abort and cleanup.

Distinguish poll frames, transient construction, and synchronous checkout work.
A debugger exit zero is not a test pass. If the inputs or phase differ, reconcile
them before interpreting the trace. No ptrace-policy change, unrelated-process
attachment, global stack increase or new compiler route is authorized here.

## Approved repair strategy

If the observation retains the known preparation/publication overlap, replace
the tail-await from initialization preparation with a private affine handoff:

- Settled: existing KernelStep, including workbench/replacement early paths.
- Program: boxed existing ResidentOutcome plus optional
  PreparedInteractivePublication, to be consumed by ordinary stabilization.
- Installer: commit-only prepared installation for the explicit-installer path,
  preserving its already completed publication/receiver work without replay.

The outer initialize owner awaits preparation to completion and drops that future
**before** polling the continuation. Move the exact public owner, source lease,
bootstrap, tool/prompt/checkpoint custody and outstanding receiver state; never
reconstruct them from a name or broader inventory. Heap-own the large transfer at
issuance so enclosing orchestration enums do not all inherit its inline size.

Both phases stay under the SAME initialization registration, cleanup guard,
compiler owner, selected context, task and serialized actor borrow. Settle custody
and disarm only after final success. Error, cancellation, dropped waiter and panic
must retain the existing cleanup obligations and publication order. No detached
task, new cancellation token, parallel actor-state mirror or scheduler.

This cut is expected to remove the completed initialization poll (about 497 KiB
in compile55) from generation publication's active chain. Measure the candidate;
that expectation is not acceptance.

If the new suffix still dominates, the next approved *investigation* is the
existing PreparingActorGeneration → complete_actor_generation_world transition:
can its first future finish before compiler completion starts, carrying the same
boxed preparation/commit owner? Do not implement both cuts speculatively. A
synchronous oversized checkout result needs a measured payload ownership repair,
not more async forwarding wrappers. Escalate a need for new execution semantics.

## Acceptance

Compile actor/host libraries and affected tests. Review every handoff variant,
early error and drop path. Measure the whole observed constructor/poll chain,
including its new suffix and wrapper, with meaningful margin on the actual
default thread stack. Keep layout measurements distinct from stack reservations.

The unchanged facade_introspection_service_test must execute and pass 1/1 on the
default stack, including its queries, publication and stale-view refusal, with
process/compiler cleanup. Then run notebook4, queries2 and applicable existing
initialization cancellation/cleanup controls on the joined source. Merely passing
the old fault instruction or accepting a larger test stack is insufficient.
