# Foundation transfer and next design review

Working checkpoint, 2026-09-29. This is not a completed handoff or launch approval.
The source-box record below is historical. Destination results and remaining
limits are recorded in [engine-foundation-destination.md](engine-foundation-destination.md).
The user wants the next xhigh ownership/publication review on the new 128 GiB
build server. Latest steering prioritizes transfer soon: finish a bounded repair
checkpoint here and move expensive joined checks to the destination. Do not
enable concurrent private executions before that review.

## Pushed WIP checkpoint

The user requested that in-progress work be committed and pushed for transfer.
The following branch tips are published; this is candidate work, not a completed
integration or release:

| Repository | Branch | Code revision |
| --- | --- | --- |
| Tidepool | `foundation/integration` | `24516ebb5f29a7e9a5f3245c485b578f31f1658f` |
| Tidepool | `foundation/module-product` | `fbcfb02027632e2a53199c0a6ef6843f5cc2a577` |
| Tidepool | `foundation/transitive-binding-retention` | `41583b5eb89737a2dd966908adbfd29bf55452ec` |
| Tidepool | `foundation/harness-m1-adapter` | `e4bb3ddc4b59f01acedcc28b9950377a2299f38a` |
| Codex (`inanna-malick/codex`) | `foundation/client-settlement` | `c09c2b067774be4104fad878ee271a6a14f12690` |
| Harness | `integration/actor-admission-companion` | `c485edb9b697ffc671b22c9ef25a73fc84763d76` |

Tidepool main carries this transfer record. The adapter branch may have a later
documentation-only handoff commit; its code revision above is exact. Adapter
Cargo files pin the harness companion above. The client pin is not yet updated
in integration. Module-product and adapter are dependent branches, not disjoint
patch stacks: inspect ancestry and apply only missing commits when joining.

The transitive binding-retention repair was omitted from the original transfer
and subsequently pushed and remote-verified by the source owner. Apply only
`41583b5eb89737a2dd966908adbfd29bf55452ec`; equivalent
`20a031feb964b8d1437b5a5f8c6537ff1d48550c` must not also be applied. Its three
regressions must run against the joined engine.

The final cancellation repair preserves an owner's completed reply through the
harness scheduler's existing terminal arbitration. Request snapshots now use a
short actor admission lease. The companion's owner-completion race passed 1/1;
the adapter's retirement race passed 1/1 (549 skipped) with the matched battery.
These repairs still need final independent review and joined verification.
No build is required on the source box before transfer. ABI regeneration,
structural gates, Nix hashes and the next xhigh design review belong on the
destination. Preserve source-box worktrees; no live sessions are transferred.

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
| Sequential embedding | Original adapter wake 1/1 and companion 5 focused tests passed; final pushed repairs and two race checks listed above | Final repair review, Nix hashes, owner composition and full M1 gates remain |

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

Adapter review found that mapping `WorkbenchCancellationOutcome::Expired { reply }` to
`Stopped` can overwrite a completed success with `Cancelled` in the harness
scheduler. The pushed repair retains that outcome and fences request snapshots
against retirement; review its exact implementation and tests before joining.
The earlier happy-path wake test alone did not establish either property.

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
