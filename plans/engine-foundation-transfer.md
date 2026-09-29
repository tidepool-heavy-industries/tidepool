# Foundation transfer and next design review

Working checkpoint, 2026-09-29. This is not a completed handoff or launch approval.
The user wants the next xhigh ownership/publication review on the new 128 GiB
build server. Latest steering prioritizes transfer soon: finish a bounded repair
checkpoint here and move expensive joined checks to the destination. Do not
enable concurrent private executions before that review.

## Source and integration

- Main planning baseline: `a2e98636b`.
- Integration branch: `foundation/integration`, currently
  `24516ebb5f29a7e9a5f3245c485b578f31f1658f`.
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
| Git admission/helper | Repairs `479a801bb`, measurement `85139acf6`, integrated; 9 admission and 4 integration checks passed after repair | Joined checks; synthetic spawn results in `foundation-view-spawn-measurement.md` |
| Compiler producer proof | `fbcfb02027632e2a53199c0a6ef6843f5cc2a577` on `foundation/module-product`; hidden-source fat/skinny comparison passed, final roundtrip 1/1, retained-scope 1/1, worker built | Final skinny simplification not joined; durable encoding/versioned imports/cache/demand consumers are not implemented |
| Client settlement | `c09c2b067774be4104fad878ee271a6a14f12690` on `foundation/client-settlement`; final focused run 13/13 passed | Commit must be transferred and pinned; broad client suite not run |
| Sequential embedding | Adapter `fc916b091cf4367ae79588d3695ed7696aa05867`: real resident/Engine wake 1/1, lib compiled; harness companion `0fa0caf410667e35ab34d11c231a71ba2b0d246d`: 5 focused tests passed | Review found cancellation outcome loss; bounded repair in progress. Request-start/retirement admission needs checking. Public Git pin/Nix hash and full M1 gates remain |

The final home-product fixture's skinny interface is 2,336 bytes versus the
earlier fat 2,747 bytes. Both forms passed source-hidden typechecking; home
products now omit redundant Core. External package Core recovery is unchanged.
This is one fixture's size measurement, not an aggregate memory or speed claim.
The producer proof has no durable neutral-group codec or cache consumer yet.

The portable retained-symbol probe is committed at integration HEAD: 1/1 passed.
G3 interface construction took 178,481 / 175,808 / 200,776 ns with 0 / 1,000 /
10,000 unrelated retained symbols; allocations were below counter resolution.
Whole requests still grew to about 309 ms. Fixture differences prevent an
absolute before/after speed claim; see `engine-foundation.md`.

## Destination verification queue

All seven registered embedded CBOR artifacts still contain execution ABI 7;
current sources require ABI 8. The existing embedded checker only checks schema
and therefore cannot certify this migration. Regenerate via the registered
producers; never edit ABI bytes by hand. No broad gate has passed on this join.

After reviewing/joining final candidates and resolving companion dependencies:

1. Use the destination's real pinned submodule checkouts, not source-box symlinks.
2. Run `env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER bash scripts/dev-shell.sh scripts/embedded-fixtures-update.sh`.
3. Check all seven envelopes and their focused consumers: repr
   `execution_schema_contract` Haskell fixtures, toolchain exact artifact linking,
   codegen freer continuation/collection, runtime freer/import/direct-global
   prepared execution and the prepared resident composite lifecycle.
4. Run full `just fixtures-check` (no cohort restriction). If corpus metadata
   needs regeneration, use its producer and review the resulting diff.
5. Finish joined codegen/runtime coverage and portable Cargo/Nix pins. Treat
   local Git URL redirects used for candidate tests as temporary evidence only.

Adapter review: mapping `WorkbenchCancellationOutcome::Expired { reply }` to
`Stopped` can overwrite a completed success with `Cancelled` in the harness
scheduler. Preserve the actual completed outcome. Request snapshot admission
also needs a defined ordering against actor retirement. Neither concern is
resolved merely by the earlier happy-path wake test.

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
