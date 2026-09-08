# Observations from the parallel application/STG run

Run: `b93ee212-a20d-4e9c-9d13-fb6d77275393`, session
`tidepool-astra-planner-20260908`. These are observations and next-prompt proposals,
not changes to the frozen instructions or claims of completed product acceptance.
The human authorized external monitoring and direct TUI steering of the Sol owner.

## What is working

- One managed Astra planner, a Sol coordinator, two Sol leads and recursive Sol
  children. Planner checked the leads' understanding, then waited on watches.
- Native active steering was presented to a working engine lead; the lead repaired
  its scaffold and sent source/check evidence back. Full acceptance remains open.
- Per-response native usage records cover the eleven observed model actors. In
  one consecutive interval, 45 requests were all Sol: 5,422,486 input tokens,
  5,365,632 cached, 10,209 output. Astra usage did not increase. This is about
  99% input cache reuse, not a dollar-cost estimate. Prior plan authoring and the
  external supervising conversation are excluded.
- The dedicated trace directory does not yet cover all child actors. Native
  rollout usage records support the figures above; complete normalized-request
  evidence for cross-child prefix reuse has not been established.

## What is slowing useful delivery

Both lanes were still in their initial A0/M0 work at this observation. Real PTY
fixtures, independent outcome classification and cost baselines are substantial
work; neither complete feature can be inferred from their scaffolds.

Workers also performed broad reads, used selected context for observed coding
forks, and grew large contexts. High cache reuse makes size alone an insufficient
failure signal, but the shared-context fork benefit remains underused.

An unused-public-helper finding caused a scaffold repair. The replacement wires
private vocabulary into engine_review.rs and passed two focused tests. Review
should judge the coherent scaffold plus assigned consumer, not require every
intermediate commit to independently complete the feature.

At one process snapshot, multiple workers compiled large Tidepool targets. Two
identically displayed `.shoal/build/cargo` paths resolved through their process
mounts to distinct directory inodes. Rustup Cargo and Nix Cargo both appeared.
This establishes isolated build roots and inconsistent invocation, not the exact
fraction of time spent recompiling or the optimal cache-sharing policy.

## Steering sent and proposed prompt improvements

Two ordinary TUI messages went to the existing Sol coordinator, preserving planner
scope and acceptance. The first was visibly presented; the second's final handling
must be checked. They ask the owner to:

- Schedule against actual dependencies; assess independent M1/M2 and A4 work
  rather than treating the complete baseline phase as a global barrier.
- Establish useful source/semantic context before inherited sibling forks;
  use selected contexts for divergent work and independent review.
- Keep implementation and focused validation with the build owner. Reuse valid
  exact-revision evidence; repeat checks for changed integration or real gates.
- Use owning Nix/just commands consistently. Test-name filtering reduces execution,
  not necessarily compilation. Avoid production API changes for fixture bookkeeping.
- Keep prompts stable, reads scoped, and retained-worker updates limited to source
  and decision changes. Preserve unresolved questions without repeatedly expanding
  unchanged records into context.

Do not add a new build scheduler, arbitrary shared writable targets, reporting
roles or frozen-prompt reloads on the strength of this observation. Next useful
comparison: does steering produce earlier independent implementation, checked
integrations, fewer duplicate builds and economical later context transitions?
