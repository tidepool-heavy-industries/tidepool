# Native-authored declaration reuse: remaining compiler boundary

Status: freshly reproduced at `20f61b99db`; phase tracing at `923cf48862`
establishes loss of the consumer's exact Original use during simplification.
The implementation strategy below is under review. G2 remains unqualified.

## Confirmed current boundary

In the same consumer request `b1af9b89ff31e82f`, on the same scaffold:

- Before simplification: `Lib.G2.historyCaptured`, GHC Name `r18MN`, has exactly
  one issued Original reference (raw compiler.jsonl line 16761).
- After simplification: `Val.G1.historyValue0`, Name `r18r5`, is present without
  an Original reference (line 16794). The 28-identifier census has no omissions,
  and `historyCaptured` is absent. Tidy and finalized output retain that state.
- Both `Lib.G1` and `Lib.G2` remain in the available native input association
  list throughout these phases. Another association-propagation path is therefore
  unnecessary for this counterexample.

The producer request `d8bd3a7531e71871` already contains `Val.G1.historyValue0`
inside `Lib.G2` before simplification. That is a different compilation and must
not be confused with the consumer's before/after comparison. Val bindings are
retained values, not source Originals.

The before census contains 70 identifiers with six omitted. The relevant
positive Original association is explicitly present; absence claims are based
only on the complete post-simplification/tidy censuses and final projection.
Tracing observes Names and existing references without acquisition, and is
disabled through the ordinary logging configuration.

Evidence: `/srv/swarm/evidence/inanna/c-ownership-20261023/audits/native-use-events-923c.json`
contains exact outer request IDs, source identity, raw trace path and line numbers.
The validator retains its corrected interpretation in `923c-checkpoint.json`.

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

## Discriminating observation used

The actual published-capture history was rerun on the repaired joined worker.
The bounded, correlated observations cover the existing compiler
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

The preferred next design extends the existing immutable retained-unfolding
policy with an authored-module boundary issued by `WorldPlannedDeclarationCompile`
and the actual stage owner. Do not infer it from `Lib.G*` spelling or a consumer's
imports. Carry the same per-module policy into both the plugin fingerprint and
memo validity; the current retained-symbol set alone cannot describe this new
policy. Preserve ordinary SOURCE and package compilation.

Evaluate pinned GHC's `OPAQUE` semantics rather than merely adding `NOINLINE`:
[GHC 9.12.2 documents](https://downloads.haskell.org/ghc/9.12.2/docs/users_guide/exts/pragmas.html#opaque-pragma)
that OPAQUE also prevents specialization and worker/wrapper replacement of named
calls. Validate the actual producer interface and consumer Core. Do not assume
that removing an early unfolding proves the final interface or call is opaque.
Ordinary value binders, constructors, dictionary functions and generated methods
need explicit treatment; an exported-name-only set may omit internal methods
that close over retained values. Constructor and dictionary representation must
remain valid. This is the unresolved semantic design question, not permission
to disable optimization globally.

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
- Strict functions, trivial aliases, authored INLINE/SPECIALIZE declarations,
  captured values behind dictionary methods, and constructor/newtype use preserve
  the intended boundary. Check emitted interfaces and consumer Core as well as
  execution; the one alias reproduction cannot establish all of these cases.

This is a distinct frontier exposed after selected-row ownership was repaired.
Do not interpret the phase migration's source/cfg passes as qualification of it.
