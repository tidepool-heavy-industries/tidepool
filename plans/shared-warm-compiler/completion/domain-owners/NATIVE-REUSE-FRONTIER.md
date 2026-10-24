# Native-authored declaration reuse: remaining compiler boundary

Status: `47739467ae` repairs the original captured-value continuation. The
joined `14c9b59df9` also repairs descriptor-driven prefix retention and preserves
stage-local TYPE context. Compilation/execution of those changes is pending;
see [STATE.md](STATE.md) for the exact validation matrix. G2 remains unqualified.

The prefix failure exposed a distinct ownership error: a source-less declaration
Join's descriptor carries implementation dependencies which its GHC `mi_usages`
does not contain. `retainInstalledInterfaceInputs` retains the selected captured
closure's actual compiler objects without rehydration. The stage TYPE repair
carries the updated WorldPrimaryInputs pair rather than borrowing its input
projection and later consulting an older context. Neither repair changes
SOURCE/native authority or permits execution from interface custody.

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

Earlier end-of-pipeline evidence is retained at
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/b493-world-projection-events.json`.
The same-request before/after evidence above now distinguishes optimizer loss
from missing initial association. Observing Names and existing references must
not acquire dead uses or choose an owner from a broader inventory.

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

## Selected design and alternative

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

The implemented candidate extends the existing immutable retained-unfolding
policy with an authored-module boundary issued by `WorldPlannedDeclarationCompile`
and the actual stage owner. `ModuleUnfoldingPolicy` couples the module's retained
identities with the closed ordinary/native definition policy. The plugin,
dependency witnesses, executable version and memo keys consume that same value.
Resolved inputs extend the existing context rather than reconstructing it and
losing the boundary. The old retained-only validity accessor is removed.

The stage owner already couples planned source, purpose and captured recipe.
Downsweep consumes captured bytes; the plan also checks the actual target summary
against its issued module before target compilation. This assertion is not before
every qualification step: the earlier checked recipe supplies that identity.
Ordinary SOURCE/package producers keep their own policy. Changed worker bytes
already change compiler producer identity; old Worlds cannot silently become
new-policy products.

Evaluate pinned GHC's `OPAQUE` semantics rather than merely adding `NOINLINE`:
[GHC 9.12.2 documents](https://downloads.haskell.org/ghc/9.12.2/docs/users_guide/exts/pragmas.html#opaque-pragma)
that OPAQUE also prevents specialization and worker/wrapper replacement of named
calls. Validate the actual producer interface and consumer Core. Do not assume
that removing an early unfolding proves the final interface or call is opaque.
Ordinary value binders, constructors, dictionary functions and generated methods
need explicit treatment; an exported-name-only set may omit internal methods
that close over retained values. Constructor and dictionary representation must
remain valid. The candidate protects ordinary top binders, including executable
default methods, while preserving DataCon, DFun and ClassOp representation.
It updates RHSs and preserved unfolding metadata through the same GHC substitution.
This is under semantic test, not yet established by the alias reproduction.

Explicit SPECIALIZE and RULES are independent of normal inline activation.
The native producer pass removes its authored rules and explicit specializations
before optimization, preserving imported rules and representation BuiltinRules.
This intentionally suppresses optimization hints within the native boundary;
it does not promise identical performance or honor unsound user rewrite rules.
GHC desugaring precedes this pass, so tests must inspect whether a compulsory
default or newtype dictionary already exposed a captured value. Do not broaden
retained authority to conceal such a failure.

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

## Current ownership and acceptance order

- Root owns GhcPipeline policy propagation and joined review.
- `audit_haskell_dead_work` owns the existing plugin and actual Core/interface,
  representation and reuse controls. No parallel compiler-policy store.
- `w2_request_protocol` owns real session histories for default methods,
  dictionary recursion, explicit specialization, shadowing and no effect replay.
- `w3_service_owner` owns generated build/resource projection for new fixtures,
  separately from its service-runner closeout.
- `validation_resume` compiles the joined worker first, then runs the original G2
  counterexample with phase tracing; broader controls follow on the joined pin.
- `astra_compiler_cutover_review` independently checks producer/plugin/consumer
  semantics. Confirmed source findings are repaired before semantic acceptance.

Read-only design evidence:
`/srv/swarm/evidence/inanna/c-ownership-20261023/audits/g2-policy-touchpoints-3e6.md`
and `g2-opacity-rules-design.md`. Compiler/plugin oracles and runtime values must
both pass: value equality alone does not prove exact issuer retention.
