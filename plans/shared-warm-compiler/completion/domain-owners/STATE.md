# Current implementation and qualification

Updated 2026-10-23. **Option C implementation is authorized. Regional closeout,
G2 and G3 remain unqualified.**

Integration: /srv/swarm/checkouts/tidepool-domain-runtime-join-20261023,
branch integration/g2-domain-runtime-20261023. Code baseline: 9cbdbf44d8.
[REQUEST-SERVICE-LIFECYCLE.md](REQUEST-SERVICE-LIFECYCLE.md) is the current
implementation contract, worker partition and gate sequence.

## Latest actual evidence

At 9cbdbf44d8, 16 selected cases produced 15 executions: **12 passed, 3 failed**.
One warning case never executed because its owned compiler failed before readiness.

- SOURCE transport/framing: 6/6 pass.
- Actual mixed-stage sealing: 1/1 pass; former WrongThread reproduction cleared.
- Materialization/promotion: 4 pass, 1 fail at a later incorrect refusal-order oracle.
- Ingress: retired CLI refusal passes; new target/Turn test uses nonexistent CLI
  activation-preview syntax rather than the typed production builder.
- Activation: actual execution fails with native demand after semantic refusal.
- Warning startup: process cleanup confirmed, compiler cleanup report unconfirmed;
  provider acquisition record independently reports76 roots preboot-released.

The two affected targets compiled at this pin. No full35-target pass is claimed.
Earlier 101/120 regional results span different revisions and are not current
qualification.17 regional cases plus the warning first execution remain pending.
Exact source, commands, counts and evidence:
/srv/swarm/evidence/inanna/c-ownership-20261022/validation/9cbd-checkpoint.json

## Confirmed boundaries being repaired

1. Haskell does not share semantic refusal state before another SOURCE borrower
   queries; Rust correctly refuses that next query.
2. World resolver retains the precise semantic error, but transport-first early
   return discards it. Failure can also precede a cancellation checkpoint.
3. Owned service reporting starts too late and skips explicit settlement on early
   errors. Partial/acquired package custody can be lost before HANDOFF publication.
4. Two test oracles encode unsupported CLI/traversal assumptions.

The Nix dnix/store error is unexplained; all76 observed argv roots were absolute.
Source inventory/compiler graph redesign is not part of this wave.

## Preserved migration and frontier

Scoped source audit found no finite notebook execution route. Ordinary native
bootstrap/installers, parser-only cell planning and typed activation preview remain
supported. Mutable-session API change has no obsolete immutable alternate route.
Purge and executable qualification are separate obligations.

The last actual G2 published-capture history failed at later reuse of
historyCaptured with UnavailableOriginalHomeDependencies for Val.G1.historyValue0.
It has not been rerun on the current candidate. See
[NATIVE-REUSE-FRONTIER.md](NATIVE-REUSE-FRONTIER.md). No bundle or Persona claim.

## Build ownership

Warm checkout tidepool-latent-original-stage-join-20261020 remains held at9cbdb
with no active commands. Validator must explicitly release before source changes.
Scratch is about104GiB; preserve100GiB plus growth. Do not open cold worker builds.
Root consolidates reviewed work and resumes bounded joined validation when feasible.
