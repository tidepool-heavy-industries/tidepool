# G2 activation placement and candidate evidence

Base: `af0ac243411e64dcadf07f201b2d352382cefdca` (code `babd430413`).
Integrated candidate: `277e589ef07f1452f2f8f39e2dd1048c915eb4b0`.
Branch: `integration/g2-domain-runtime-20261023`.
Evidence: `/srv/swarm/evidence/inanna/g2-activation-candidate-20261025/`.

Argon accepted the previous memo HOME and SOURCE-dependent load-graph repair.
This checkpoint implements the two reviewed ownership repairs and corrects the
diagnostic reader's interpretation of relocated preparation captures. G2 remains
unqualified; source review and executed acceptance are separate below.

## Integrated changes

Runtime commit `78157b9a3f` exposes the preview owner's recorded `mounted_scope`
within the session module. Publication compares that scope with the private
execution and separately checks that the detached source-custody scope is live.
The detached snapshot and existing issuer, epoch, binding, visibility and
interface checks remain intact. Tests add a real foreign execution admission,
explicit detached-versus-mounted scope assertions, and retirement of the
detached scope while the private scope remains live, including frozen-intent
replay refusal.

Haskell commit `090905cb5f` carries each accepted candidate and its admitted
`PackageImportEvidence` together from `pprCandidateAdmissions` into production
certification. That route no longer reopens or decodes candidate companions.
Standalone encoders read captured tokens when custody exists, retaining the
fresh-file route, digest checks and bounded interface/package reads. Standalone
tests use compiler-issued interface/sidecar bytes and captured request inputs,
companion alias drift and refusal controls. The joined Rust case exercises the
production opaque-candidate admission route.

Diagnostic commit `f9e37e7d10` associates a prepared capture with a unique
completed capture in the same case using bounded, stable request and working-
directory content. Association requires complete discovery, valid completion
facts, no capture errors or pending marker, and exactly one preparation and one
completion. Missing, empty, ambiguous, symlinked, nonregular or oversized input
evidence refuses association. It preserves `phase=prepared` and
`submission=not_attempted`, adds the associated completion path, and never
infers physical attempt counts or rewrites compiler outcomes.

The six previous diagnostic gaps were not truncated stderr. Their nine prepared
records all have a unique corresponding completed request in a separate output
directory: `prepare_admitted_turn` relocates outputs before freezing the request,
while preparation diagnostics remain with the input directory. All prior streams
were complete. `diagnostics/prior-evidence-final-reanalysis.json` uses the final
reader to reconcile those records without changing physical counts or original
receipts. This is read-only reanalysis, not a new workload execution.

## Verification state

Independent source review accepts both repairs and the diagnostic reader;
`review.md` records exact revisions and limits. The diagnostic unit suite passes
144/144 cases, including ambiguous/missing completion, differing cwd, interrupted
capture, empty or invalid files and discovery bounds. Evidence is in
`diagnostics/unit-final.json` and its log.

The runtime production library and test target compile. Exact native discovery
identified the real `session::resident::activation_input_tests` paths. Both
focused runtime tests executed and failed before reaching the repaired guard:

- `durable_activation_binding_retains_private_authored_source_after_retirement_and_collection`:
  the unchanged `before_selection.inherited().is_empty()` assertion at
  `activation_input_tests.rs:5325` fails during initial fixture setup.
- `frozen_activation_replays_same_input_and_refuses_second_private_input`:
  `begin_fixture_execution` at line 523 reports
  `MissingPublishedCompilerWorld(ScopeId(1))`. The retained error does not
  distinguish its first foreign-admission call from the subsequent matching call.

These failures do not establish execution of the new positive or refusal
assertions. Receipts and raw streams remain in `runtime/controls/`.

The full prepared-STG suite at `090905cb5f` executes 13 cases: 12 pass and
`original constructor reply identities and type evidence` fails at
`TypeEvidenceChecks.hs:211`. The full source-boot suite executes 120 cases:
99 pass and 21 fail. The changed `original package projection` case and all four
memo/SOURCE controls pass. Exact failing leaf names and exceptions are retained
in `haskell/source-boot-failure-census.json`; a wrapper display-only UTF-8 error
did not discard the raw log or the actual subprocess verdict. Outer logs are
complete, but fixture-owned failure snapshots retain at most 32 files,
8,192 bytes per file and 128 traversal entries; diagnostic display prefixes
are limited to 4 MiB. `haskell/report.json` records these limits, exact binaries,
baseline comparisons and idle daemon shutdown.

The integrated matrix at `277e589ef0` executes twelve cases:

| Selection | Passed / executed |
|---|---:|
| Captured candidate companions after alias drift | 1 / 1 |
| Private-host custody | 2 / 3 |
| World TYPE histories | 2 / 2 |
| Actor notebook contracts | 5 / 5 |
| Ordinary dependency reuse | 1 / 1 |
| Total | 11 / 12 |

The candidate case establishes the production certification route after alias
drift. Both private-input retirement controls pass; activation fails at
`resident_workbench.rs:34195`, with
`Resident(InvalidActivationInput { site: 8364848696887532212 })` from
`frontend.mount_activation_input(...).await.unwrap()`, after input capture.
The failure is the second replacement mount. Retained `cell-prepared.cbor` and
`cell-plan.cbor` contain the first iteration's `activationResult` consumer with
`sessionInput (5 :: Int) == 6`. That consumer occurs only after successful
publication and the exact visible-binding assertion. Reaching the next mounting
call proves that consumer returned, so the first publication crossed the
repaired guard. `joined/activation-loop-evidence.json` records artifact paths
and hashes; `resident_workbench.rs:34188–34228` establishes the ordering.
All twelve cases have complete diagnostic evidence, confirmed process and
compiler cleanup, and complete compiler cleanup observation. Exact commands,
case names, artifact hashes and physical request observations are in
`joined/report.json`; `result.json` independently censuses the twelve native
receipts. Executable and compiler-worker hashes are captured, while directory
resources still require their owning manifests. This is diagnostic execution,
not frozen-bundle evidence or G2 qualification.

Both validation checkouts are clean at their recorded revisions. Their owned,
idle Buck daemons and forkservers were stopped after checking active commands;
source, branches, outputs and toolchain roots remain retained. Haskell closeout
is in `haskell/report.json`; joined closeout is in `joined/settlement.json`.
The integrated source diff and this packet pass `git diff --check`.

Two focused comparisons at baseline `af0ac24341` each execute one case and
reproduce the same failures: prepared-STG's polymorphic answer-evidence assertion
and source-boot's `snapshot executable hook` refusal for
`main:MetadataQuoteSupport`. Those two failures predate this parcel. No baseline
execution claim is made for the other 20 source-boot failures. Logs and exact
commands are in `haskell/baseline-prepared-type-evidence.{json,log}` and
`haskell/baseline-snapshot-hook.{json,log}`.

## Review questions exposed by broader checks

1. The joined activation history now refuses during its second replacement
   mount, after the first publication and consumer succeeded. Identify the exact
   predicate through the retained transaction and owning mount path. The receipt
   does not identify which `InvalidActivationInput` producer refused. The prior
   `babd430413` run stopped at the first publication, so it did not execute this
   replacement boundary and cannot establish a regression here.
   Do not weaken input authentication to reach publication.
2. The durable fixture's global-empty premise conflicts with its real receiver
   setup. `fresh_with_library` publishes `activationReceiver` and
   `activationUnitReply`; child scope construction inherits custody-backed HOME
   selection. The independent review recommends requiring absence of the exact
   newly issued private owner from the baseline, preserving all later exact
   `(binder, SourceLeaseKey, handle)` before/after equality, domain checks and
   retirement/collection behavior. No fixture correction is included here.
3. The frozen fixture lacks a usable published World when creating its private
   execution. A refresh through the existing World constructor is not yet
   justified by an identified invalidation path. Preserve this refusal and trace
   the owning transition before changing setup; do not manufacture a World.
   `runtime/world-currency-hypothesis.md` identifies raw source installation's
   conditional visibility-epoch advance as a concrete hypothesis, not a proven
   dynamic cause. The error also covers stale, not just absent, associations.
4. The prepared-STG failure concerns the answer-evidence contract for
   `auxiliaryRootPolymorphic :: a -> a`. The existing projection strips leading
   foralls and arguments, observes free `a`, and skips answer evidence. The test
   expects a `TypeForAll` node for a closed polymorphic answer. This case supplies
   no candidate manifest, and its production projection and fixture are
   unchanged by the candidate repair; baseline execution reproduces the failure.
   The competing contract expectations need resolution; no assertion or
   projection guard has been changed. The bounded
   trace is in `haskell/prepared-stg-failure-trace.txt`.
5. Source-boot has 12 `ExecutionSourceUnavailable` failures, two
   `InterfaceUnownedThinHome` failures, three retained-execution fixture-premise
   failures, and four other leaf failures. These are diagnostic groupings, not
   established shared root causes. Only the snapshot leaf has a baseline run.
   Start with the issuing SOURCE/TYPE authority and real fixture constructors;
   preserve exact authorization and typed refusal boundaries.

The remaining broad G2 gates, frozen bundle, Persona startup and real-provider
acceptance remain governed by
`plans/shared-warm-compiler/completion/g2-completion.md`.
