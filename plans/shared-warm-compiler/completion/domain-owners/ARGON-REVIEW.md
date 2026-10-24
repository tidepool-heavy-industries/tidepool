# Argon adversarial review: compiler ownership closeout

Review packet, 2026-10-23. **G2/G3 remain unqualified.**

Checkout: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`
Branch: `integration/g2-domain-runtime-20261023`
Code candidate: `dc34094d5e7135d221b09a5d24e5047a4eb5e4e3`.
Runtime diagnostics were executed at parent `f8051efccd`; the only later code
change is the owning toolchain test's fixture correction. That correction
compiled and its owning case passed at `dc34094d5e`.
Baseline for this repair tranche: `09c4aaf3dec573fe97dadb857709e7eb8b60244c`.
Broader native-opacity change: `fb56a5c9d6` + `47739467ae` and subsequent tests.
Broader Option C contract: [REQUEST-SERVICE-LIFECYCLE.md](REQUEST-SERVICE-LIFECYCLE.md).

## Review mandate

Challenge the production dataflow, invariants and test premises. Do not accept
source-review endorsements as evidence of behavior. Trace issuance, transfer,
selection and consumption; distinguish an actual bug, an invariant enforced by
another owner, an unclear boundary, and an untested case. Recommend deletion or
a narrow owning type when it removes reconstruction/plumbing. Backward
compatibility may break; no dual legacy/new path is desired.

Primary focus is the two newly exposed product boundaries below. Also inspect
whether the recent repairs preserve authority and whether the tests exercise
actual production owners. Return exact locations, minimal counterexamples,
severity, discriminating tests, and a bounded design recommendation. Do not
silently fix failures by widening inventories, inferring ownership from names,
accepting equal bytes as an allocation witness, or treating interface custody as
execution permission. Review existing code, not just the diff.

## What this tranche changed

- Native-authored declaration policy is issued by the actual planned stage,
  propagated once, and included in module reuse fingerprints. Ordinary top-level
  executable definitions are OPAQUE; DataCon/DFun/ClassOp representation remains.
  Explicit native authored rules are suppressed; imported rules remain. Actual
  GHC Core/interface/reuse test passes after its input was made genuinely unknown.
- Prefix retention formerly reconstructed all required homes from mi_usages.
  A source-less declaration Join intentionally has no ordinary usage list, while
  its certified descriptor retains the original implementation closure. The
  repair selects captured requirements first, then retains actual installed
  compiler objects through ExactHydration.retainInstalledInterfaceInputs.
  Completed session/finalized rows retain their issued HMI allocation; decoded
  ordinary rows require an existing matching HMI. No hydrate/retie fallback.
- Stage compilation borrowed checked TYPE rows but discarded their updated
  context. DemandedWorldCompilerInputs now retains the WorldPrimaryInputs bundle.
  Source admission consults that exact stage context; persistent continuation
  and lexical/instance scope still derive from the prior primary plus the
  admitted fragment. Exact descriptor and lexical claim checks remain.
- Obsolete tests treated INLINE native declarations as SOURCE producers. They
  now test exact native historical capture, declaration custody, actual demand,
  dead-use exclusion, and no effect replay. Genuine SOURCE owner/transport tests
  remain separate. SourceQueryObserver and old names are removed.
- CertifiedTargetOwners previously collapsed dozens of guards into one unit
  error. It now retains the originating Rust Location without changing failure
  classification. This is diagnostic evidence, not a root-cause repair.

## Current product frontier A: imported thin interface ownership

At 14c9b59df9, both class-default histories advance through declaration and seed
shadowing, then fail during later instance compilation with
`InterfaceUnownedThinHome (main, Tidepool.Session.Val.G1)`. The adjacent same-cell
history reaches the same boundary. This is a refusal introduced by the new
retain-only operation for a previously decoded zero-fingerprint interface;
zero fingerprints cannot prove which actual HMI allocation is owned.

Trace these existing owners before proposing a change:

- GhcPipeline.WorldCellResolvedInputs already retains HydratedExactInterfaces.
- World import/operand projections also retain paired hydrated owners.
- WorldCellInputs / selectedTypeBodies currently exports only
  [(ExactIfaceArtifact, ArtifactBytes)] for completion.
- retainWorldCellOriginal reads that byte projection back through
  readVerifiedExactIfaceClosureWithCheckedValuesReader, then adds only the
  directly injected/produced session captures.
- A transitive thin interface can therefore reach retention as decoded data,
  despite successful earlier hydration. Direct captures and completed products
  retain richer ownership.

Working hypothesis: a live installed-interface association is lost when the
completion projects to transport bytes. Carry the actual selected association
through the completion; keep byte projection for transport consumers. Determine
which owner can do this without retaining an entire stale HscEnv or choosing an
arbitrary same-name HMI. Do not simply remove the thin-input refusal, compare
zero fingerprints, rehydrate missing objects, or resurrect a second cache.
This hypothesis needs source/evidence verification; no repair is claimed.

## Current product frontier B: native retained-value installation

At 14c9b59df9, G2 reaches Prepared(CertifiedTargetOwners) after **all nine compiler
transactions succeeded**. Two opacity histories similarly reach their final
visible replacement-value observation before that refusal. At `f8051efccd`, both
G2 and the renamed historical-native case failed at
`tidepool/runtime/src/session/admission/scoped_inputs.rs:331:36`: the fallback
after no exact selected NativeOriginal terminal matched. This identifies the
guard, not which side of its OR predicate failed. The specialization history
was not rerun with this diagnostic; do not silently group it under the same cause.

A source-review hypothesis for G2 is scoped_inputs.rs resolve_native_requirement:
Acquired slots first seek a request/stage-bound NativeOriginal terminal. If none
matches, the fallback accepts an exact acquired slot only when the VALUE is
Produced in the current World. An older Admitted value captured by a newly
compiled declaration has a different age, despite potentially valid fresh
native-group issuance. NativeCodeGrant::Home already distinguishes product and
current. If confirmed, model the demander's issuance rather than infer authority
from the value's age. Keep historical terminal stage, demander, global ordinal,
full binding and checked artifact identity checks. Do not search arbitrary old
stages or reinterpret SOURCE terminals as native ones.

Other sites compare target/code ownership, import alignment, runtime value
identity/representation, evaluatedness, and native installation witnesses.
Use the originating guard before choosing a repair.

## Evidence and limits

All checkpoints and exact command/artifact records live at:
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/`.

- 09c4: eight targets compile; 18 native/Tasty leaves = 11 service/worker passes,
  seven compiler/test-premise failures. Separate runner-refusal controller passes
  while its child remains unqualified (native PASS, outer runner exit 1).
- 4c79: diagnostic opacity leaf fails unchanged predicate; actual consumer Core
  proves constant folding, 163 + 6*y. Stable unfolding text was not actual RHS.
- 14c9: five targets compile; seven executed = opacity Tasty PASS, G2 and five
  runtime leaves FAIL at later boundaries. Earlier ChangedSynthetic and Join
  closure refusals cleared. Two new FatIface leaves deferred after source review
  caught use of a fat interface where TYPE admission intentionally excludes Core.
- 893b: two targets compile; corrected FatIface ownership leaves PASS 2/2.
  Existing World counter case FAILS its explicit nonzero-coverage assertion.
  That counter exposed a fixture-premise mistake, not a proven product bug.
  Operand input is already in continuation.primary; the repaired prior-visible
  binding path is different. `dc34094d5e` corrects construction via an actual
  sealed value and visible-host-binding API, with no explicit operand. It asserts
  the TYPE is absent from primary, the value is actually demanded, and the
  exact/omitted/changed counter increased. No new authority API was added.
  The owning cfg target compiled and that case then passed at dc340, including
  the mandatory completed-control counter. Exact/omitted/changed borrowed-TYPE
  controls now have actual owning-test execution evidence.
- f805: runtime cfg compiled; two runtime cases executed and both failed at the
  same guard above. Process and compiler cleanup confirmed, hosted cleanup not
  observed, diagnostic completeness false. Those are independent facts.
- dc340: toolchain owning cfg compiled; one World value-selection case executed
  and passed with outer runner acceptance. No runtime or unchanged service
  cohorts were rerun for this test-only correction.

Important retained artifacts:
- 14c9-g2-prepared-artifact-index.json (38 files with hashes).
- 14c9-actual-published-capture-g2-one-native-use-events.json.
- 14c9-runtime-native-opacity-four.log.
- 09c4-typed-captured-closure-mismatch.json and full compiler.stderr path therein.
- 893b-world-borrowed-type-owner-one.log; 893b-checkpoint.json.
- f805-checkpoint.json; f805-final-owner-artifact-index.json;
  f805-final-owner-guard-diagnostics-two.log. The artifact index preserves
  compiler certificates, metadata, traces and per-case reports.
- dc34-world-borrowed-type-owner-one-summary.json, per-case report and log.

Implementation/review records (source evidence, not acceptance):
`/srv/swarm/evidence/inanna/c-ownership-20261023/audits/`
contains prefix-captured-closure-652af.md and g2-opacity-fixture-repair-09c4.md.
`/srv/swarm/evidence/inanna/domain-astra-tests-20261023/retain-installed-owner-68c86.md`
records bounded owner controls. g2-borrowed-type-admission.md under c-ownership-
20261023 records the corrected cfg(test) coverage claim.

## Test contracts to challenge

Actual G2 history captures 23, publishes an Original, observes it twice, inserts
an unrelated binding before a new declaration that captures the older value,
then consumes that declaration again. No effect replay, no replacement binding,
no unrelated native demand. It is the real descriptor-producing regression.

The two retained-interface leaves use real compiled or session-issued captures.
They prove actual HMI allocation retention, completed allocation preference over
an equal-interface decoded allocation, absent decoded-HMI refusal, wrong NameCache,
unowned thin refusal, unrelated HPT exclusion, and successful separate hydration
of the same ordinary bytes. They do not establish that every live production
completion carries the needed owner (frontier A demonstrates otherwise).

Runtime tests link the production toolchain library: they cannot execute its
cfg(test) hooks. The owning toolchain test must require an executed-control
counter, not infer negatives from a runtime pass or a missing log. Explicit
counter checks must survive the fixture repair.

Native capture migration preserves exact artifact/group identities, retired
roots and no-replay observations. It removes a false SOURCE premise; it does not
claim a working retained-value SOURCE producer. Ordinary closed SOURCE startup
explicitly refuses retained capture imports.

## Requested output

1. Accept/refuse each repaired ownership boundary with concrete reasons.
2. Diagnose or discriminate frontiers A/B independently.
3. Propose the smallest coherent next tranche with coupled owning types and
   production-path tests; name APIs/alternate routes to delete.
4. List incidental quality/performance/diagnostic findings separately from
   release blockers, with evidence. Identify any test that proves the wrong
   property or silently lacks coverage.

A passing component result does not establish main/release readiness, frozen
bundle acceptance, G3 qualification or Persona readiness. The user selected
Argon for this adversarial review; implementation workers should finish their
bounded handoffs, not start another independent review swarm.

Suggested prompt:

> Adversarially review the pinned code and surrounding production consumers using
> this packet. First challenge the repaired ownership boundaries and test premises;
> then independently diagnose the two remaining frontiers. Return grounded findings
> and a Pareto set of bounded next-tranche designs that carry issued authority in
> types and delete obsolete routes. Distinguish confirmed defects, containment by
> another layer, unclear contracts, and untested hypotheses. Do not implement yet.
