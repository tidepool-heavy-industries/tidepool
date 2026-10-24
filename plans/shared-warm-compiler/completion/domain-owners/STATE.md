# Current implementation and qualification

Updated 2026-10-23. **Option C source is consolidated; joined validation is
pending. Regional closeout, G2 and G3 remain unqualified.**

Integration: /srv/swarm/checkouts/tidepool-domain-runtime-join-20261023,
branch integration/g2-domain-runtime-20261023. Code candidate:
06ecb784efe8fce44d1bf5c60419059c95f3cd48; baseline: 9cbdbf44d8.
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
Its 18-target owner/downstream compile passed; 18 focused native cases have
passed so far, with the remaining component checks running at that fixed pin.
The actual Rust-writer-to-Python-reader check failed for all five emitted reports:
the reader requires a 192-character endpoint, but the production issuer emits 64.
This is product contract drift, not an invalid Rust fixture. Logs and exact build
evidence are under /srv/swarm/evidence/inanna/c-ownership-20261022/validation/.

The newest source additionally includes typed source admission and failed-recovery
custody repairs. Those require compilation and execution after the validator
releases 0bbc6ebbf5. Do not run G2 on the older pin or relabel its passing evidence.

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
- Joined report boundary: W4 is correcting endpoint length and valid live Pending
  report parsing, with a maintained actual Rust writer/Python reader control.
  W5 is migrating compiler-warm-contention.py from the retired lifecycle format.
  The service reviewer must recheck their combined result.
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
validation_resume for compilation and focused execution at 0bbc6ebbf5;
other workers must not mutate it. Scratch cleanup reclaimed 110.33 GiB, leaving
about 214 GiB free at the last observation. Preserve 100 GiB plus expected growth
and recheck actual headroom before admission. Sources, WIP and evidence survived.
Cleanup report: /srv/swarm/evidence/inanna/c-ownership-20261023/storage/report.txt.
