# Retained-value TYPE transport: integrated candidate for Argon

Code candidate: `5e10df3bef27417b338f292720648b61c06c9925` on
`integration/g2-domain-runtime-20261023`, in
`/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
Baseline code: `2e60aa8101`; implementation contract:
[g2-value-type-implementation-20261024.md](g2-value-type-implementation-20261024.md).
Evidence root: `/srv/swarm/evidence/inanna/g2-value-type-20261024/`.
Consolidated receipt index: `integrated-results.json` under that root.

The locked retained-value TYPE product and transport changes are implemented.
The original retained-Job actor regression passes, as do the constructor/cache,
transport, Haskell lifecycle, and compiler corpus controls below. Joined actor
validation has exposed constructor correlation, SOURCE/TYPE admission, and HOME
linked-code identity frontiers. No guard has been weakened. G2 remains unqualified.

## Requested review

1. Verify the complete value-plus-TYPE product through issuance, cache reuse,
   wire admission, selected hydration, and last-reference retirement. Pay special
   attention to the two implementation findings described below: unchanged
   request cache ownership and scope-change cold reconstruction.
2. Diagnose the nominal successor's new constructor refusal after exact bindings
   agree: distinguish selected-row facts, producer identity, and payload role.
3. Diagnose fresh SOURCE compilation overlapping an operand's exact TYPE owners.
   Determine the missing authority-preserving transition, if any; equal source
   bytes alone do not authorize replacement of an exact owner.
4. Diagnose concurrent publication of the same physical HOME issuance with
   different linked-code identities. Distinguish legitimate target-specific
   linking from lost canonical code ownership before proposing a repair.

## Implemented boundary

`SelectedCompilerInterfaces::checked_value_type_closure` correlates the selected
leaf and every transitive Interface edge with authenticated ScopeProofV3 bindings.
It checks exact owner/requirement sets, producer/interface seals, and cycle-safe
binding traversal. Canonical, ValueInterface, and LexicalJoin dependencies are
supported; dependency-only rows confer neither SOURCE nor runtime-value authority.

`CheckedValueArtifact` retains an opaque complete physical TYPE product and a
separate freshly demoted TYPE projection. The physical product has no ViewLease
or CompilerSourceSupport back-reference. Existing materialization and produced-row
owners validate the whole expected closure before reuse, preserving first issuance
and receiver-before-ancestor cache locking. No parallel cache was introduced.

One encoder owns the five-field row fact:
`[semantic_row, leaf_binding, certified_artifact, receipt, dependency_closure]`.
Cold World/restoration use version 6; both successor writers use `TPCWSUCC6`;
operands use `TPCWCELLOPERAND2`. Old leaf-only serialization is removed. Dependency
rows use full ArtifactBindings and existing InputAcquisition/file custody.

Rust and Haskell count dependency users by distinct live row-fact CID. Alias
changes are completed before deriving net inserted/retired CIDs. Positive-to-
positive transitions preserve physical custody without transport edits; the last
reference emits Null and releases custody. Existing charge/budget owners account
for dependency rows and manifests.

Haskell keeps historical dependency custody outside the nominal base/visible-row
indexes. Only demanded World bindings and the operand contribute to hydration.
Binding plus canonical semantic equality is checked before owner collapse;
physical path aliases are accepted. The returned selected TYPE map includes both
leaves and dependencies, including the case where one selected leaf is another
selected value's transitive dependency.

## Additional findings repaired within the boundary

**Unchanged request cache ownership.** `CompilerSourceSupport::close_request`
previously reconstructed an equal projection in a temporary descendant view.
Materialization populated that temporary cache rather than the retained issuer.
The corrected lifetime fixture reproduced recopy at baseline before the fix.
The equal-projection path now returns `support.resolve_input_policy()`; role
merging/conflict checks precede it and changed-projection validation is unchanged.
Independent source review found no authority bypass. DurableFiles controls retain
their fresh-owner requirement and exercise actual RecordedRoles ingress.
See `p1/report.json`, `p1/complete-issuer-baseline-tests.log`, and
`p1/accepted-product-results-final.json`.

**Compiler query scope changes.** A successor belongs to its held scope. When the
query scope changes, `project_bindings_with_primary` now reconstructs a cold World
in a new directory, preserving the predecessor's descriptor and custody. Same-
scope changes retain incremental encoding. The discriminator checks the actual
wire and predecessor immutability (`p2/handoff.json`).

## Executed evidence

These are separate component/joined receipts at their recorded sources, not a
single frozen release-bundle qualification.

| Check | Executed outcome | Source and evidence |
|---|---|---|
| Rust product, exact graph, cache, physical lifetime/refusal controls | 24/24 pass; production and cfg(test) compile | P1 `251aac4633`; `p1/report.json` |
| Rust transport DATA controls | 4/4 pass | Exact sources and binary hashes in `p2/handoff.json` |
| Genuine nominal and captured transport histories | 2/2 pass | P2 final `31a2beab17`; `p2/handoff.json` |
| Genuine historical same-owner custody | 1/1 pass | `p2/historical-nominal-final.log` |
| Haskell owned inputs, nominal lifecycle, actual fixture issuer matrix | 3/3 leaves pass; issuer leaf includes 30 generated histories | Final production core at `899a5074be`; `haskell-tests/result.json` |
| Haskell captured and historical selection/operand histories | 2/2 leaves pass | Exact `5e10df3bef`; `haskell-tests/captured-historical-final.log` |
| Compiler fixture corpus | 12/12 cohorts, 1,052 programs pass; opacity probe passes | `25be28a2ae`, tree identical to `f8834ad8fe`; `p3/fixtures-check-final-core/result.json` |
| Retained Job actor regression | 1/1 pass, result 42, original job awaited without replay | P4 `17c7d9e326`; `p4/captured-job-thunk-fixed/` |
| Existing actor notebook cohort | 5 executed: 3 pass, 2 fail at HOME linked-code checks | `17c7d9e326`; `p4/notebook-summary.json`, `p4/home-code-identity-review.txt` |
| Corrected nominal/promotion and operand actor histories | 2 executed: 0 pass, 2 fail; later promotion stages not reached | Exact `5e10df3bef`; `p4/value-type-histories-corrected/`, `p3/actor-histories-5e10-controller/` |
| Generated registrations | Complete graph check and 61 generator tests pass | Exact `5e10df3bef`; `generator-final-check.log`, `generator-final-tests-5e10.log` |

All production changes are present in the final-core corpus and actor candidates;
subsequent changes through `5e10df3bef` are tests, fixture helpers, and registration.
The corrected nominal/operand actor pair is recorded separately below.

The captured TYPE discriminator is genuine B -> Lib -> A: A has a nominal home
type, the declaration captures A using `seq`, and B's nominal type requires that
declaration. B's product contains A's exact ValueInterface binding, and the
canonical declaration explicitly requires A. Merely assigning `B = input` was
rejected as an insufficient test premise.

Historical tests use independently compiled `Token` and `Token Bool` versions of
the same module. Passive custody admits both; separate demands resolve different
authenticated interface bodies; simultaneous World/operand demands refuse the
exact conflict. A successful retry proves refusal leaves the predecessor usable.
Same-payload/different-selection decoder controls are labeled DATA, not forged
Rust authority. Physical-alias controls compare exact row CIDs and selected maps.

The lifecycle oracle parses raw rows, CID manifests, unique file paths, and part
sizes independently. Cold and successor histories can retain different physical
path sharing: measured charges `(14282,38)` versus `(11411,32)` have independently
measured physical portions `(9743,12)` versus `(6872,6)`, and equal logical residual
`(4539,26)`. Tests preserve exact semantic identities and nonnegative accounting
rather than requiring identical physical storage between those routes.

## Frontier A: fresh SOURCE overlaps an exact TYPE owner

The actual operand-only JSON fixture admits its own dependency closure without
installing a primary observation scaffold. Operand admission succeeds, but native
preparation refuses `fresh module replaced an admitted exact owner` at
`ProductInputFacts::admit_source_facts` in `declaration_context.rs`.

The retained operand includes `Tidepool.Aeson.Value`, `Tidepool.Aeson.Scientific`,
`Tidepool.Data.Text`, and `Tidepool.Double`, plus its Val.G1 leaf. The cold prototype
and consumer SOURCE inventories overlap all four canonical TYPE owners, not
Val.G1. This rules out a generated value-module collision as a necessary cause.
See `p4/operand-type-source-overlap.json` and
`p4/value-type-histories-final/` for the initial executed case; the corrected final
candidate run is `p4/value-type-histories-corrected/`.

Exact `5e10df3bef` reproduces the same refusal: the no-primary premise and actual
operand parser admission pass, then `prepare_checked_cell` refuses before its
prepared-input assertion or operand execution. All six compiler transactions
completed successfully. Exact stages/errors are in
`p3/actor-histories-5e10-controller/case-results.json`; the particular rejected
owner has not been instrumented.

A separate toolchain reproducer reaches the same guard: a real nominal value and
`CellProgram.item_successor`, followed by a declaration that captures the operand
and explicitly imports its module as SOURCE. Retained source bytes are identical
across producer/consumer (SHA256
`1a9b453ba6c731e26bc051cfac8cb89546a55a8af757fb3ffd177869a96f46f9`).
Exact binding and transaction evidence are in `p2/captured-frontier-handoff.json`.
Removing the second explicit SOURCE import gives the passing B -> Lib -> A TYPE
discriminator above; this is a controlled scope distinction, not a guard bypass.

The guard intentionally rejects fresh modules overlapping checked-value exact
owners. Its World exception authenticates prior-program SOURCE; it does not turn
an arbitrary TYPE dependency into SOURCE authority. Hypothesis: qualification is
recompiling captured/template SOURCE that needs an exact Original reuse/association
transition. This remains unconfirmed. The rendered refusal does not identify the
individual owner; the intersections above come from retained inputs/inventories.

## Frontier B: same HOME issuance, different linked code

The existing notebook case
`resident_local_actor::completion_progress::parked_cell_does_not_block_another_calls_acknowledgement_or_control`
completes A's items and the B/C progress checks, then fails when A resumes public
promotion (`completion_progress.rs:121`):
`VersionSkew: artifact inventory: acquired HOME binding has competing linked code`.
Two committed receipts are retained; publication fails after two units.

In `AcquiredNativeClosures::same_home` (`artifact_inventory/compiler_projection.rs`),
this refusal occurs only after the full binding, `PendingCertifiedGroup::same_issuance`,
and `IssuedSelectedNativeGroup::same_selected_issuance` checks pass. Therefore the
observed mismatch is `prior.code().id() != incoming.code().id()` for the same
physical issuance, distinct from the previously repaired competing-import-Arc
failure. This source deduction does not identify the particular group or the
different code-identity inputs yet.

An adjacent executed refusal in
`reload_progress::unavailable_reload_leaves_parked_cells_and_control_independent`
is `selected HOME head changed its exact closed code`. That path relinks selected
plans and compares a prior HOME head with the new `NativeCodeRef` before reusing
its authorizing occurrences (`compiler_projection.rs`, near line 1288). Do not
infer the full `same_home` predicate sequence from this second error alone.

`NativeCodeRef` equality compares its ID; the ID hashes component identity and
member ordinal (`tidepool/repr/src/native_code.rs`). `NativeOccurrencePlan::link`
constructs plans from ordered owner-derived slots before calling the component
linker. Comparing component facts, member locations, and exact child code choices
is a useful next discriminator; no claim of hash instability is established.

First receipt:
`p4/notebook-contract/3585106ed2f70a22d577957c616b0630e8c353ee8baa7e2bf494b03e7c91f062/`.
No baseline replay or instrumented identity-pair capture has been claimed. Possible
causes include target-dependent linkage inputs or loss of canonical linked-code
ownership across concurrent branches; neither is established. Preserve exact
issuance and code-identity checks while tracing the producer/merge contract.

## Frontier C: nominal successor reaches the new constructor guard

On exact `5e10df3bef`, the corrected actor case
`world_checked_nominal_values_retain_type_closure_through_private_promotion`
passes its initial absence-from-primary assertions and executes the ordinary
nominal producer. Its next cell (`world-nominal-private-consumer.hs`) fails with
`checked value TYPE dependency differs from its selected interface`. Promotion,
public reuse, and declaration stages are not reached; they remain unqualified.
All seven captured compiler transactions completed successfully. The refusal is
on the Rust constructor path, not a GHC syntax error or failed fixture premise.

The issuing branch in `SelectedCompilerInterfaces::checked_value_type_closure`
(`compiler_projection.rs`, near line 2599) is reached after both namespace and
selected-interface ArtifactBindings equal the traversed binding. It then refuses
one of three conditions: selected entry `same_issued_facts` mismatch, descriptor
producer unequal to the value interface's toolchain identity, or unsupported
payload role. Exact binding mismatch has a separate structured error and should
not be inferred from this generic diagnostic. The particular disjunct and owner
are not yet established. This is the priority implementation review question.
`ArtifactEntry::same_issued_facts` includes physical facts-owner identity, as well
as descriptor and requirements equality (`artifact_inventory.rs`, near line 2489).
Consequently equal bindings do not by themselves prove this check passed; inspect
whether the selected projection and inventory payload retained the same issuer
before interpreting the refusal as a changed interface or toolchain.

The corrected fixture supplies `RetainedNominal` through captured workbench
imports, not an authored import-declaration item. The earlier explicit import
had legitimately added primary rows and invalidated the original no-primary
premise; that fixture mistake was fixed before this execution. Preserve the
new failure as a separate production-path observation.

Evidence: `p3/actor-histories-5e10-controller/nominal-first-failure.json` and
`p4/value-type-histories-corrected/4301afca863f92aa3a4e41a9ad345047e4679ee416570e1c9b483756f9ab0268/`.
Actor executable SHA256:
`9cbeb6f3e7dc8a09d340342b30bf37145405a52396f3b353fd55f0e32f8f1de1`.

## Qualification and cleanup limits

The retained-Job pass includes explicit fixture shutdown and process/compiler
cleanup. Its generic hosted cleanup telemetry is `not_observed`, because that
fixture does not emit the host-runtime report; it is not claimed as observed.
Component workers stopped their inspected idle owned Buck daemons and retained
source, products, and evidence. Notebook process/compiler cleanup is confirmed
for all five cases; generic hosted telemetry is not observed. The notebook runner
also marks diagnostic evidence incomplete, so the retained exceptions establish
the narrow refusal sites, not a complete linkage trace. Final actor runner cleanup
is confirmed for both corrected cases at the process/compiler level, with complete
diagnostic capture and hosted telemetry unobserved. All worker-owned idle Buck
daemons were stopped after inspection; source and outputs remain retained.

Pending release work remains in `g2-completion.md`. This packet establishes the
implemented TYPE boundary and discriminating evidence; it does not supersede
failed joined actor gates, prior Q10/activation/reload obligations, or the required
single-bundle G2 acceptance. Review these three frontiers before extending the
implementation into additional authority or native-linking changes.
