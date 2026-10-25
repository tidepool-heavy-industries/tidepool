# G2 current producer and activation scope repair

Base: `e0685304e2` (code `a9791613e4`, production changes `c9bc31eb6b`).
Branch: `integration/g2-domain-runtime-20261023`.
Review: `/srv/swarm/evidence/inanna/g2-current-producer-20261025/argon-review.md`.
Evidence: `/srv/swarm/evidence/inanna/g2-current-producer-20261025/`.

## Current producer membership

An Original retained for recovery can share its canonical interface with a new
RetainedCore product while having a different Original ArtifactId and version.
Both must remain in admitted custody. Enumerating that custody as current stage
production makes `WorldStageNativeProducts::capture` compare the historical
Original against the new product's receipt selected by nominal module name.

The toolchain change under implementation derives Original producer membership from the exact completed cohort
already issued by `RequestNativeAvailability::from_current_cohort`. Each expected
ID must resolve through `ArtifactAdmission::producer_entry`; no raw-entry or
nominal-owner fallback is allowed. Admitted non-Original certificate handling
remains intact. Carried CompletedPrior/Input products remain custody; current
production comes from PendingCurrent completion. Physical issuer and native group
checks remain required.

The regression must independently establish distinct old/new Original IDs with
the same module and canonical interface, retain both physically, and assert only
the current product enters the stage producer census. Preserve equal-ID authority
refusals and the emitted-but-unselected canonical control.

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

## Acceptance

- Compile toolchain production/cfg(test), run the actual acquisition controls and
  historical/current producer regression, and verify the regression fails before
  the production repair.
- Execute the activation-publication case through its owned compiler, then the
  integrated nominal/operand histories and private-host custody controls.
- Run the five-case notebook target at the integrated source and compile affected
  host consumers. Keep compilation separate from execution evidence.
- Record exact revisions, commands, artifacts, counts and cleanup in the evidence
  reports. G2 remains unqualified until the remaining completion gates execute.

Previous notebook 5/5 and operand passes remain bound to `c9bc31eb6b`; the corrected
fence control and final host compilation remain bound to `a9791613e4` in the prior
`g2-type-frontier-followup-20261024/` evidence. They are not rerun results for this
candidate. Inventory identity and live World decoder redesigns remain separate.
