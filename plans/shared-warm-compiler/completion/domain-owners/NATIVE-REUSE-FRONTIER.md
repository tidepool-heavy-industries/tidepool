# Native-authored declaration reuse: remaining compiler boundary

Status: root investigation, not an accepted implementation expansion.
Observed at de968 and trace-enabled b493. G2 remains unqualified.

The 2026-10-23 review against `1de912c683` confirms that the relevant ordering
and reference construction are still present. This is source evidence, not a
fresh reproduction. The purge candidate `33ae06028c` must first compile and run
the joined controls; its current Haskell compile repairs do not establish that
this runtime failure is resolved.

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
or a remaining exact Original use. These counters do not establish whether the
association was absent before optimization or whether optimization erased a use
whose association was originally available. Inlining/alias propagation is one
source-supported explanation, not a confirmed cause.

## Next discriminating observation

Rerun the actual published-capture history on the repaired joined worker. If it
still fails, retain bounded, correlated observations at the existing compiler
phases: before simplification, after simplification, and after tidy. For the
relevant free identifiers, record the actual GHC Name identity, native-authored
classification, and exact `referenceForName` result. Include the owning module
and fresh/reused compilation route. Use opt-in structured tracing; observing
references must not perform native acquisition or change compiler inputs.

- Association missing before simplification: trace the actual admitted
  `nativeOriginalInputs` and World import/Original association. Repair that
  propagation before changing optimization.
- Association present initially, then absent with a changed free-identifier set:
  locate the transformation and evaluate the producer-owned opaque boundary
  below. GHC may legitimately erase an ordinary Haskell alias; the harness must
  preserve its required native boundary or carry its exact provenance.
- Association survives through tidy: inspect projection and retained-generation
  admission; the empty-reference explanation no longer fits that run.

The diagnostic does not authorize acquisition of dead uses, matching by rendered
names, or choosing a historical owner from a broader inventory.

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
