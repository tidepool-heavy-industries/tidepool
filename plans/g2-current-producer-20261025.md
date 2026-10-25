# G2 current producer and activation scope repair

Base: `e0685304e2` (code `a9791613e4`, production changes `c9bc31eb6b`).
Branch: `integration/g2-domain-runtime-20261023`.
Review: `/srv/swarm/evidence/inanna/g2-current-producer-20261025/argon-review.md`.
Evidence: `/srv/swarm/evidence/inanna/g2-current-producer-20261025/`.
Integrated code: `5cb4ed9ec8a6043d70f37070f12675a96b7e808d`.

## Current producer membership

An Original retained for recovery can share its canonical interface with a new
RetainedCore product while having a different Original ArtifactId and version.
Both must remain in admitted custody. Enumerating that custody as current stage
production makes `WorldStageNativeProducts::capture` compare the historical
Original against the new product's receipt selected by nominal module name.

The implemented change derives Original producer membership from the exact
completed cohort already issued by `RequestNativeAvailability::from_current_cohort`. Each expected
ID must resolve through `ArtifactAdmission::producer_entry`; no raw-entry or
nominal-owner fallback is allowed. Admitted non-Original certificate handling
remains intact. Carried CompletedPrior/Input products remain custody; current
production comes from PendingCurrent completion. Physical issuer and native group
checks remain required.

The regression independently establishes distinct old/new Original IDs with
the same module and canonical interface, retains both physically, and asserts only
the current product enters the stage producer census. Equal-ID authority
refusals and the emitted-but-unselected canonical control remain covered.

## Activation fixture scope and corrected diagnosis

`8a1b542527` resolves `activationReceiver` in the fixture's actual notebook scope
and retains that exact binding through `retain_binding_custody_in`. The fixture
asserts its non-ROOT scope premise. Temporary constructor trace loops are removed.

The prior missing-payload diagnosis was incorrect. Its panic location came from
`c9bc31eb6b`, but investigation used the newer file after a ten-line shift. At the
recorded revision, `live_payload_handle` succeeded and the failing inner `unwrap`
was the ROOT-only `retain_binding_custody("activationReceiver")` lookup. The
superseding `fixture-audit.md` records the exact revision/tree and numbered source
excerpt, plus a bounded audit of the other ten actor callers, all intentional
ROOT cases. Historical evidence remains retained outside the checkout; there are
no named scratch notes to delete from the clean source tree.

## Evidence and remaining qualification

`toolchain/report.json` retains the component commands, executable hashes and
counts. At test-only `a6e0ce16e4`, the strengthened acquisition case executes and
fails with two Original producer IDs instead of the expected singleton after
establishing both actual custody owners. At `43e4e39eb4`, production/cfg(test)
compilation and all five focused controls pass. Its toolchain source matches the
integrated `5cb4ed9ec8` exactly. Independent source review is retained in
`source-review.json`.

The integrated nominal/operand pair passes 2/2, including the final thin
declaration consumer. Receipts are in `actor/joined-5cb/`.
The integrated private-host cohort records 2/3 passed: both retirement controls
pass, while activation repeats the later HOME refusal. Receipts are in
`actor/private-host-5cb/`. The notebook target passes 5/5 at the integrated
revision (`notebook/report.json`). Required actor cfg(test), host production and
host cfg(test) targets compile successfully; this is compilation evidence, not
host test execution. `actor/joined-result.json` retains the commands, artifact
hashes, counts and terminal build result.

Process and compiler cleanup is confirmed for all ten integrated cases; hosted
cleanup is not observed. Both validation checkouts are clean at the tested
revision, and their owned idle Buck daemons and forkservers have been stopped.
Source, branches, outputs and evidence remain retained.

The activation case at `8a1b542527` executes and advances past the corrected
receiver lookup, then fails later in a standalone request-result fixture.
`actor/activation-result.json` records 0/1 and confirmed process/compiler cleanup;
hosted cleanup is not observed. This is a new failure boundary, not a successful
activation-history run. The integrated `5cb4ed9ec8` cohort confirms the same
failure at `request/test_support.rs:135` after the corrected scoped lookup.

## Next review boundary: ordinary source memo and installed HOME

The later activation failure is in the second compilation of
`request/test_support.rs`'s immutable result fixture: the producer
`result-request.hs`, after its receiver compiled successfully and before the
result fixture's actor machine is created. It uses ordinary `run_turn` with no
exact World and with authenticated ordinary module candidates. The Haskell worker
refuses with `completed source version lacks installed HOME main:Tidepool.Actor`.

The source bytes match the recorded inventory. `Tidepool.Actor` is absent from
the 20 ready candidate owners, while prior execution evidence records it as
interface-only. Several checks use `CompletedModuleHomeMissing`, so the retained
diagnostic does not yet identify the throwing call. The bounded lead is memo/HPT
correlation across candidate graph deferral and later source completion, not a
proven fixture admission error.

`new-activation-home-frontier.md` and `new-activation-home-source-extract.txt`
retain the exact `8a1b542527` source excerpts, receipt, candidate evidence, competing
explanations and minimal discriminator. The next observation should identify the
check site and the module's selected/validated memo, load graph, deferred and HPT
membership before and after Make. Preserve the actual installed-HMI requirement;
finalized interface bytes cannot replace its owner.

The integrated failure receipt is
`actor/private-host-5cb/2410948d234b6d7a78aedcd6d7a98536382817429b8a3dce90821aab539d3d25.json`.
Its retained transaction is `.tmph5MZyC` and compiler-failure directory
`.tmpKaIfuV`, under that receipt's `artifacts/` directory. The fixture and
GhcPipeline source are unchanged between the initial and integrated executions.

G2 remains unqualified until this history and the remaining completion gates
execute successfully. No Haskell source or HOME refusal was changed in this repair.

Prior receipts remain bound to their historical source revisions in
`g2-type-frontier-followup-20261024/`; the new results above are independently
executed at this candidate. Inventory identity and live World decoder redesigns
remain separate.
