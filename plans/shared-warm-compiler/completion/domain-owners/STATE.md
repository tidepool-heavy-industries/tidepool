# Current implementation and qualification

Updated 2026-10-23. **Option C source is consolidated; joined validation is
pending. Regional closeout, G2 and G3 remain unqualified.**

Integration: /srv/swarm/checkouts/tidepool-domain-runtime-join-20261023,
branch integration/g2-domain-runtime-20261023. Code candidate:
20f61b99db; baseline: 9cbdbf44d8.
[REQUEST-SERVICE-LIFECYCLE.md](REQUEST-SERVICE-LIFECYCLE.md) is the current
implementation contract, worker partition and gate sequence.

## Integrated source and current checks

W1 worker refusal lifecycle, W2 consuming request completion/protocol ownership,
W3 service retirement, W4 durable package acquisition, and W5 consumer/oracle
repairs are integrated. The final callback BEGIN guard and report-codec packaging
are included. Root reviewed those final deltas and the pinned Buck bootstrap
implementation; full-wave diff check passes. This is source integration, not
execution acceptance.

At the earlier 9e2abcd26b checkpoint, the production worker built, WorkerResponse
passed 11/11 and direct ingress passed 5/5. No owned service ran in that check.
W4's 68 Python tests and W5's runner/generator checks passed on their parcel
revisions; they do not qualify the joined Rust/Haskell service.
Evidence: /srv/swarm/evidence/inanna/c-ownership-20261022/validation/9e2a-w1-checkpoint.json.

The first d387e2f488 joined compile exposed a deleted configuration helper still
used by two property tests. The bounded repair is integrated at 0bbc6ebbf5.
Its 18-target owner/downstream compile passed. Fifty native tests executed:
47 passed and three failed. Two failures were transport-fixture premises;
one configured-provider fixture omitted required receipt fields and never reached
the intended profile-decoder refusal. The separate actual Rust-writer-to-Python-reader
check rejected all five reports because the reader required a 192-character
endpoint while the production issuer emits 64. That was product contract drift.
All four mechanisms have reviewed repairs in the current candidate. Their native
reruns remain pending. Three packaging CLI probes passed separately; they are
not executed test cases. Exact evidence is under
/srv/swarm/evidence/inanna/c-ownership-20261022/validation/.

The newest source additionally includes typed source admission and failed-recovery
custody repairs. The next compile at 9a35539e9c caught one missed caller passing
the captured-source owner to a byte comparator. The compiler rejected this
migration before execution. Root repaired that caller at 20f61b99db, reading
path and bytes from the same captured owner; joined compilation is being repeated.
Two stronger test-only refusal oracles are also included. Do not run G2 on an
older source candidate or relabel earlier passing evidence.

Root's lightweight checks at 9a35539e9c passed: 11 contention cases, 35 report/package
cases, and 141 isolated-runner cases. The parcel's reported 12-case contention
check was 11 contention cases plus one separately selected runner case.
Logs: /srv/swarm/evidence/inanna/c-ownership-20261023/w5/joined-9a-*.log.
The first direct-file runner invocation failed import before execution; its
correct module invocation is recorded in joined-9a-runner-module.log.

## Open follow-ups

- Compiler recovery: independently reviewed b702238b9d is integrated, including
  real uncached backing controls for propagated, swallowed and replaced failures.
  Final scope settlement carries the actual failed recovery assessment and owns
  cleanup reporting. Follow-up 256e854b18 deletes the obsolete raw source capture
  API. New Haskell controls still need execution.
- Source admission: independently reviewed typed witness plus registered
  owned-resident controls are integrated through 67cf9ddb20. The old native raw
  digest accessor is deleted; canonical identity remains distinct from admission.
  Rust compilation, paired compile contracts and execution remain pending.
- Joined report boundary: endpoint identity and live Pending parsing are repaired;
  the maintained Rust writer/Python reader control covers eight actual reports.
  Its Pending refusal changes only retirement state from a valid complete report,
  with an accepted incomplete counterpart. Contention uses the shared reader and
  distinguishes incomplete, unknown, workload failure and retirement failure.
  Independent source review accepted the joined consumers and final oracle fixes.
  Native execution of the new report control remains pending.
- Provider child lifetime: a controlled actual-process reproduction confirmed a
  Nix client can survive Python-provider death. W3 is implementing a bounded
  parent-death contract for those exact subprocess edges, with explicit limits:
  killing a client does not prove descendants reaped or shared-daemon work settled.
  Missing provider completion continues to retain unknown acquisition custody.
- Argon's additional dead-code/equality proposals remain investigations, not
  implemented changes or G2 prerequisites. Findings and rejected hypotheses are
  retained in /srv/swarm/evidence/inanna/c-ownership-20261023/audits/PEER-AUDIT-REVIEW.md.

## Previous executable frontier

At 9cbdbf44d8, 16 selected cases produced 15 executions: **12 passed, 3 failed**.
One warning case never executed because its owned compiler failed before readiness.

- SOURCE transport/framing: 6/6 pass.
- Actual mixed-stage sealing: 1/1 pass; former WrongThread reproduction cleared.
- Materialization/promotion: 4 pass, 1 fail at a later incorrect refusal-order oracle.
- Ingress: retired CLI refusal passes; new target/Turn test uses nonexistent CLI
  activation-preview syntax rather than the typed production builder.
- Activation: actual execution fails with native demand after semantic refusal.
- Warning startup: process cleanup confirmed, compiler cleanup report unconfirmed;
  provider acquisition record independently reports76 roots preboot-released.

The two affected targets compiled at this pin. No full35-target pass is claimed.
Earlier 101/120 regional results span different revisions and are not current
qualification.17 regional cases plus the warning first execution remain pending.
Exact source, commands, counts and evidence:
/srv/swarm/evidence/inanna/c-ownership-20261022/validation/9cbd-checkpoint.json

## Mechanisms addressed by the integrated wave

1. Share semantic refusal across valid SOURCE borrowers while preserving gate checks.
2. Consume resolver completion and preserve semantic and transport causes together.
3. Own service reporting and acquisition custody before launch, including failures
   before HANDOFF publication; require actual retirement evidence for release.
4. Replace unsupported CLI/traversal test assumptions with production-issued inputs.

The Nix dnix/store error is unexplained; all76 observed argv roots were absolute.
Source inventory/compiler graph redesign is not part of this wave.

## Preserved migration and frontier

Scoped source audit found no finite notebook execution route. Ordinary native
bootstrap/installers, parser-only cell planning and typed activation preview remain
supported. Mutable-session API change has no obsolete immutable alternate route.
Purge and executable qualification are separate obligations.

The last actual G2 published-capture history failed at later reuse of
historyCaptured with UnavailableOriginalHomeDependencies for Val.G1.historyValue0.
It has not been rerun on the current candidate. See
[NATIVE-REUSE-FRONTIER.md](NATIVE-REUSE-FRONTIER.md). No bundle or Persona claim.

## Build ownership

Warm checkout tidepool-latent-original-stage-join-20261020 is held by
validation_resume for compilation and focused execution at the current candidate;
other workers must not mutate it. Scratch cleanup reclaimed 110.33 GiB, leaving
about 214 GiB free at the last observation. Preserve 100 GiB plus expected growth
and recheck actual headroom before admission. Sources, WIP and evidence survived.
Cleanup report: /srv/swarm/evidence/inanna/c-ownership-20261023/storage/report.txt.
