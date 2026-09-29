# Foundation transfer and next design review

Working checkpoint, 2026-09-29. This is not a completed handoff or launch approval.
The user wants the next xhigh ownership/publication review on the new 128 GiB
build server. Finish the current bounded candidates and joined checks here;
do not enable concurrent private executions before that review.

## Source and integration

- Main planning baseline: `a2e98636b`.
- Integration branch: `foundation/integration`, currently `591d3c04c`.
- The integration branch contains the native code/installation split, responsive
  actor control and settlement, typed source freshness, deferred external effects,
  runtime code reuse, ABI primitive repairs, admission leases, real Event
  retirement regression and installed tool snapshots. These are candidate joins;
  main has not been advanced and joined gates remain.
- Preserve all worktrees and companion repositories until exact revisions are
  committed, verified and available on the destination. Local Git objects alone
  are not a portable dependency or handoff.

## Current evidence and remaining boundaries

| Slice | Candidate / evidence | Remaining |
| --- | --- | --- |
| Runtime code reuse | `93abb8445`; one off-checkout reuse regression passed | Joined runtime/ABI gate |
| Native primitive ABI | `e7eb59627`; 10 data-tag/lifetime tests passed | Full codegen coverage and producer-regenerated artifacts |
| Short host admission | `3ed5ff603`; two admission/retirement tests passed | Embedded Store guard integration; no guard across dispatch/wake await |
| Real Event retirement | `1edde59c3`, integrated as `db9323057`; facade test 1/1 | Joined regression |
| Frozen installed tools | `af91337af`, integrated as `591d3c04c`; three focused checks passed | Embedded consumer and joined regression |
| Git admission/helper | `25f0579fc`; 4 admission tests, 6 focused worktree runs, helper build, facade 1/1 | Review repairs: lockfile/view/private-Git-dir identity, scope lifetime, joined checks, measured spawn behavior |
| Compiler producer proof | `c7dac8efd` then `6a98eab469`; module roundtrip 1/1, retained-scope 1/1, extractor build | Exact review; durable encoding/versioned imports/cache/demand consumers are not implemented |
| Client settlement | `foundation/client-settlement`, uncommitted | Last run 12/13; remaining test stopped server before mock inference request. Added wait needs rerun and final commit |
| Sequential embedding | Actor lane plus separate harness companions | Wake/snapshot/cancel API checks, real HostActor composition, portable Git pin/Cargo lock/Nix hash |

The module fixture's fat interface is 2,747 bytes, skinny HPT form 2,402 bytes.
This is one fixture's size measurement, not an aggregate memory or speed claim.
The producer proof has no durable neutral-group codec or cache consumer yet.

## Next xhigh review

Read `engine-harness-integration.md`, `harness-first-tree-prd.md`, and
`harness-integration-runtime.md`, then inspect actual final candidate sources.
Review the next execution boundary before implementing/enabling it:

1. Split per-execution state from actor lifecycle and shared coordination. Do not
   clone the whole behavior or add a competing scheduler.
2. Private declaration/value write sets retain original identities. Compiler-
   validated joins stage outside checkout and publish atomically against the
   latest public generation; effects must never replay after a stale join.
3. Cancellation and publication have one authoritative ordering point. Cleanup
   names exact owned continuations, not a session-wide before/after subtraction.
4. Reusable captures retain completed private lexical meaning independently of
   parent success. They do not freeze resources or the filesystem.
5. Durable products retain exact interface/body pairing and full positive and
   negative dependency evidence under the existing artifact/cache owner.
6. Statically reachable recursive groups compile before execution off-checkout;
   no first-use JIT traps. Catalogs must not retain every unused native export.

## Transfer completion requirements

Record full final OIDs and dependency/application order for Tidepool, client,
harness and workspace; distinguish reviewed/compiled/executed/blocked evidence.
Retain necessary diagnostics in tracked, bounded reports without private context
or credentials. Push or explicitly transfer every required Git object before
calling the handoff portable. Do not migrate live sessions, change deployment,
restart shared daemons, or launch a wave as part of this checkpoint.
