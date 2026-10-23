# Native-authored declaration reuse: remaining compiler boundary

Status: root investigation, not an accepted implementation expansion.
Observed at de968 and trace-enabled b493. G2 remains unqualified.

## Counterexample

The real resident history captures `historyValue0 = 23`, authors
`historyCaptured = historyValue0`, binds another value, observes 23, and publishes.
Those stages pass. A second cell `segmentRecord historyCaptured` fails with
`UnavailableOriginalHomeDependencies` for `Val.G1.historyValue0`.

The b493 trace records, on admission 6 / request f1468d3e86db9446:

- direct retained inputs: 0; SOURCE dependency generations: 0; inherited references: 0;
- final projection retained map: only the new `Val.G4.observation0 -> 4`;
- available exact Original reference map: empty;
- native replies acquired: 0.

Source and event locations:
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/b493-world-projection-events.json`.
The underlying compiler.jsonl lines 9964 and 10260 preserve the raw observations.
The native declaration dependency is exposed without its captured-value generation
or a remaining exact Original use. The precise GHC transformation that erased the
use is not yet independently captured; inlining/alias propagation is supported by
the source path, not established solely by these counters.

## Ownership and ordering

- `ExactScope.CanonicalOrigin` distinguishes `SourceOriginal` from
  `NativeAuthoredDeclaration`. `worldHasSelectedSource` intentionally observes only
  the first. Giving the authored declaration SOURCE rights would change authority.
- `GhcPipeline.captureSourceDependenciesFor` observes desugared SOURCE uses before
  simplification. `freezeSiblings` builds `pprOriginalReferences` from finalized
  tidy free Ids; erased native uses therefore cannot be recovered there.
- `CompilerRequest.prepareArtifactsWithProjectionIntent` constructs the retained
  generation context, then projects, then checks HOME executable globals, then
  queries exact native demand. Native replies currently never enrich projection's
  generation map. Moving only the refusal check is insufficient.
- `NativeDemandPacket` enters executable acquisition. Querying every early free Id
  would acquire dead/optimized-away calls or fail on irrelevant unavailable bodies.
- `RetainedUnfoldings` rewrites the defining module's own top binders only. Its
  module-local validity fingerprint is built around that fact. Adding imported
  identities to a target's retained set would not implement import masking.

## Design candidates to review

1. Preserve an opaque call boundary for authenticated native-authored declarations.
   Prefer enforcement at the actual producer/compilation contract, while all facts
   needed for a module-local validity key still exist. Ordinary SOURCE/package
   behavior and constructor handling must remain independent. This reuses exact
   native demand and avoids carrying transformation provenance, at the cost of
   cross-boundary optimization. Determine the smallest affected class from real
   evidence before broadening the policy.
2. Preserve exact native-use provenance through optimization, then acquire only
   reached uses and replay their retained terminals before final projection.
   This retains optimization, but requires a sound association when multiple
   Originals expose the same symbol through different historical acquisitions.
   A broad inventory or symbol-name join cannot supply that association.

A narrow reorder/reprojection is viable only when a surviving exact Original use
is available. The observed empty map makes that insufficient for this case.

Do not change global `Opt_OmitInterfacePragmas` as a shortcut: this repository's
canonical representation policy also uses it to preserve automatic constructor
unboxing (GhcPipeline.canonicalizeRepresentationFlags). Keep canonical bytes,
private Core, usage fingerprints and memo validity coherent at their issuing owner.

## Required controls for the next repair

- This actual published-capture history reaches its second observation, without
  replaying effects, and proceeds through the previous-World Original continuation.
- A dead/optimized-away native use performs no executable acquisition.
- Two exact native owners closing over different historical values cannot resolve
  by spelling, latest generation, or arbitrary contribution order.
- Direct retained values and ordinary SOURCE/package examples preserve behavior.
- Foreign/missing native acquisition still refuses; typed and native metadata do
  not confer each other's authority.
- Both initial compilation and warm reuse apply the same validity/input policy.

This is a distinct frontier exposed after selected-row ownership was repaired.
Do not interpret the phase migration's source/cfg passes as qualification of it.
