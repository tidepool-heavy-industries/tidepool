# G2 TYPE frontier follow-up and review packet

Base: `7ec29daa12700baa1b23233ca11e87056340ced3` (code `c947ed5d1e`).
Review input: `/srv/swarm/evidence/inanna/g2-type-frontier-followup-20261024/argon-review.md`.
Evidence root: `/srv/swarm/evidence/inanna/g2-type-frontier-followup-20261024/`.

Production candidate: `c9bc31eb6ba56fc74c014d4a4f2768ccb4dc7113`.
Test-only follow-up: `a9791613e4af8f6aab24572495e8e3dd156478d8`.
Branch: `integration/g2-domain-runtime-20261023`.
The nominal history still fails its final thin-declaration consumer. G2 is not
qualified; the bounded source repairs and their component controls are available
for review independently of that remaining joined failure.

## Implemented changes

- Scoped the acquisition fixture's native-row validation state to each actual
  artifact inventory, preserving shared accounting and legitimate reuse within one
  inventory. Selection IDs are content hashes at this revision, not counters;
  equal semantic bindings need not imply identical physical issuer allocations.
- Derived stage producer certificates from checked post-admission outcomes, with
  no fallback to raw pre-admission entries. Retained original ValueInterface fact
  handles where the already-issued value receipt requires them, without changing
  producer admission/transport or native witness checks.
- Added settlement of queued dropped custody at actor checkout entry before scope
  validation and read-only binding inspection. Successful release and refused
  retirement retention have separate controls.

The stage now enumerates `ArtifactAdmission::supplied` outcomes after admission,
rather than requiring an outcome for every emitted entry. An emitted but
unselected artifact legitimately has no supplied outcome. The control checks that
such an entry is absent from both stage certificates and physical custody.
Current-cohort offer issuance still requires its specific producer outcome.
Neither path falls back to a raw pre-admission entry.

The baseline acquisition cohort reproduces the shared-validator refusal. Its
second acquisition test already passes, contrary to the review's predicted
immediate failure. The strengthened equal-Original regression explicitly proves
canonicalization happened, fails before the stage repair, and passes afterward.

The auxiliary dropped-input fixture held a persistent source fence through its
final positive public query. Its refusal, retained-cleanup and scope-retirement
assertions had already succeeded. `a9791613e4` explicitly reopens that exact
fence, using its original source, after those assertions and before admitting
fresh inspection work. Dropping a `RuntimeSourceFence` does not reopen it;
production fence and retirement semantics are unchanged.

## Execution evidence

The JSON reports retain exact commands, source revisions, executable hashes,
executed counts and cleanup observations. A compile-only pass is not execution.

| Check | Source | Result / receipt owner |
| --- | --- | --- |
| Toolchain production and cfg(test) | `2cd96fed05` (integrated as `c9bc31eb6b`) | Compiled; `rust/report.json` |
| Two actual acquisition controls plus strengthened current-cohort producer control | `2cd96fed05` | 3/3 passed; `rust/outcomes-controls/` |
| Physical fact owner, outsider refusal and emitted-but-excluded control | `2cd96fed05` | 2/2 passed; `rust/outcomes-physical-fact-controls/` |
| Checkout drains dropped custody before scope validation | `a5beb6ac22` | 1/1 passed; `runtime/checkout-regression/` |
| Runtime, actor and host production/cfg(test) consumers | `a5beb6ac22` | Six targets compiled; `runtime/result.json` |
| Joined nominal/operand histories | `c9bc31eb6b` | 1/2 passed: operand passes; nominal final consumer fails; `runtime/joined-type-histories-c9/` |
| Actor notebook contract target | `c9bc31eb6b` | 5/5 passed; `notebook/report.json` |
| Private-host custody cohort | `c9bc31eb6b` | 1/3 passed; one fixture fence error and one unresolved missing payload; `runtime/private-host-custody-c9/` |
| Corrected dropped-input refusal/retirement control | `a9791613e4` | 1/1 passed; `runtime/refused-cleanup-a979/` |
| Final host production and cfg(test) consumers | `a9791613e4` | Compiled, exit 0; `runtime/final-host-consumers-a979.log`, build `72c00da0-4036-4ff7-acbd-8a9f7b3b5722` |

`runtime/result.json` and `runtime/review.md` include the corrected fixture's
exact receipt and final consumer artifact hashes. The host test executable was
compiled, not executed as a broad suite.

The joined pair and private-host cases have confirmed process and owned compiler
cleanup. Hosted cleanup telemetry is `not_observed`, not inferred successful.
All five notebook cases also confirm process/compiler cleanup. Its isolated Buck
daemon was inspected idle and stopped; source, outputs and evidence are retained.
The runtime/actor validator's owned Buck daemon and forkserver were also inspected
idle, stopped, and confirmed exited. Both validation checkouts remain clean at
their recorded source revisions; their branches, mounts and outputs are retained.
The independent bounded source acceptance in `source-review-final.json` preceded
the joined run and does not supersede its remaining failure.
`generator-tests.log` records 88 passing generator controls; that log does not
contain a source OID receipt and is not counted as exact-candidate qualification.

Previous corpus validation completed at `70e11ee38e`: 12 cohorts / 1,052 programs,
exit 0. Its source, artifacts and counts are in the prior evidence root's
`corpus/summary.json`; this does not qualify later runtime changes.

## Review priority 1: final nominal declaration consumer

Exact case:
`resident_workbench::request_tests::world_checked_nominal_values_retain_type_closure_through_private_promotion`.
Receipt: `runtime/joined-type-histories-c9/4301afca863f92aa3a4e41a9ad345047e4679ee416570e1c9b483756f9ab0268.json`.
Its 16 retained compiler transactions are beneath the matching receipt directory's
`artifacts/compiler-transactions/`.
Independent investigation: `followup/nominal-producer-investigation.md`.

The ordinary private nominal read, private promotion, producer retirement, fresh
public read and subsequent captured declaration complete. The final fresh
invocation of `world-nominal-captured-consumer.hs` fails in
`WorldStageNativeProducts::capture` with
`World stage completed product has another physical issuer`. The message combines
artifact-ID and issuing-authority predicates; it does not identify which failed.

The producer vector now comes from admitted outcomes, and the component controls
prove reconciled producer/offer sharing. A remaining hypothesis is that the
admitted outcomes contain historical recovery Originals alongside a newly
produced Original for the same nominal module. `produced_source_receipt` looks
up `(unit, module)`, while capture iterates supplied artifacts. Comparing a
historical version against the current receipt would fail before any physical
authority comparison. This needs exact transaction evidence; do not infer an
Arc mismatch solely from the error text.

Source inspection confirms that recovery products can carry historical Originals:
their deduplication uses the full `CachedHomeOwner`, including version, while
World capture resolves receipts by nominal owner. The retained declaration
transaction `.tmpnskjBD` marks `RetainedNominal` and `RetainedNominalLeaf` fresh;
the final consumer `.tmpl5k9LI` marks them retained-Core, with equal source and
interface hashes but changed dependency witnesses. Those wire facts do not reveal
the final admitted ArtifactIds or the actual failed comparison.

Please establish the actual offending owner, artifact IDs and predicate, then
pressure-test the owning distinction between current production and admitted
recovery inputs. Preserve exact producer admission/transport and group ownership.
The regression must include the historical/current overlap that the current
equal-Original component tests do not establish.

A bounded repair to pressure-test is to carry exact selection-derived current
Original membership separately from admitted recovery custody, using strict
admitted-outcome lookup for each expected current ID. Test two different Original
ArtifactIds for the same nominal owner, both retained but only one selected as
current, alongside the equal-ID/different-authority refusal. This is a design
proposal awaiting the missing observation, not an execution-qualified fix.

## Review priority 2: auxiliary activation payload fixture

Exact case:
`resident_workbench::request_tests::original_source_activation_publishes_into_recipient_world_and_replaces_input`.
Receipt: `runtime/private-host-custody-c9/2410948d234b6d7a78aedcd6d7a98536382817429b8a3dce90821aab539d3d25.json`.
Investigation: `runtime/activation-payload-investigation.md`.

The fixture reaches `live_payload_handle(submission.cont_id())`, which returns
`Ok(None)` before the activation publication assertions. The retained producer
metadata identifies `SubmitRequestWith` with its expected `StaticWithSite`
reply field 1 and payload field 2. Matching `RunRequest` constructor identities
alone does not establish the actual suspension at the failed retrieval.

Baseline status is unknown. Existing evidence does not distinguish the wrong
suspension/loop iteration, a missing parked payload, or a prior transfer. No new
`set_actor_execution` occurs between the producer resume and retrieval. The next
discriminator is the actual request constructor and iteration, capture policy,
parked-frame presence, and whether payload custody was already taken. Do not
classify this as either a production regression or a fixture bug without that
observation.

## Deferred decisions

Inventory-wide native issuer identity changes require evidence from production
callers and an explicit treatment of content-addressed recovery. Do not replace
physical issuer checks with semantic equality. A bounded independent audit is
complete at `7ec29daa12`: no production counterexample was found. Retain this as
a separate API-hardening decision, not a required wire identity change.

The live World decoder cache redesign remains separate: its owner must supply
current live custody and rebased wrappers with correct retained-byte accounting.
No decoder setting or wire identity change is part of this follow-up.
