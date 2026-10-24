# Current implementation and qualification

Updated 2026-10-23. **Option C source is consolidated and the seven repaired
regional checks pass. One runner-refusal control remains pending; G2 and G3
remain unqualified.**

Integration checkout: /srv/swarm/checkouts/tidepool-domain-runtime-join-20261023.
Branch: integration/g2-domain-runtime-20261023. Regional tested candidate:
3e6ca9b3ef; native-opacity candidate under qualification: 47739467ae.
Baseline: 9cbdbf44d8.
[REQUEST-SERVICE-LIFECYCLE.md](REQUEST-SERVICE-LIFECYCLE.md) owns the contract.

## Integrated work

- W1 worker refusal, failed-recovery custody and opaque captured-source access;
  the raw capture helper is deleted.
- W2 consuming request completion and shared protocol ownership; reentry and
  input lifetime controls retain independent cleanup uncertainty.
- W3 service retirement and exact startup refusal controls; direct provider
  children follow their parent's lifetime, including frozen-source verification.
  This does not establish Nix daemon cancellation or unobserved child reap.
- W4 durable acquisition and the shared report codec. Eight actual Rust reports
  cross the Python reader; pending retirement cannot claim completion.
- W5 contention/report consumers and separate interpreter/script arguments in
  the Haskell fixture runner.
- Typed source-admission witnesses replace raw admission-digest accessors.
  Canonical source identity remains distinct from current-source admission.

Source reviews accepted these bounded changes. Actual compilation caught a missed
captured-source caller; subsequent test runs exposed fixture and generated-target
defects. Repairs are integrated, with the remaining execution gates below.

## Executed evidence

Results are revision-specific, not a passing release aggregate.

| Revision | Observed result |
|---|---|
| 20f61b99db | 20 owner/cfg/worker/Haskell targets compiled. Three paired contracts each compiled one positive and refused one negative. 22 unique native/libtest leaves: 18 native passes, 4 native failures; 17 runner-accepted and 5 refused because one passing native assertion deliberately retained cleanup. |
| 923cf48862 | Instrumented Haskell compilation passed. G2 reproduced the retained-dependency failure. The strengthened parser positive failed on invalid layout. |
| 4755a9da61 | Buck analysis refused an omitted generated rule load before compilation/execution; repaired in d18eac6150. Joined Python provider-lifetime/argv checks passed 37 cases. |
| 3e6ca9b3ef | Five affected targets compiled. SourceBoot 4/4, parser 1/1, rooted GHC lifecycle 1/1, and mixed-stage history 1/1 passed, including outer runner acceptance. Both Rust cases confirm process and compiler cleanup; diagnostic evidence completeness remains false and is not inferred from cleanup. |
| 47739467ae | Joined native-opacity production worker and prepared-STG test executable compile. Actual G2 and the existing retained-plugin owner test are running; no execution pass claimed yet. |

Passing 20f6 controls include repaired transport 2/2, WorkerResponse 11/11,
actual report interoperability, catalog/source-admission controls and runtime END
reporting. An independent reader accepted all eight unchanged emitted reports.
A prior probe used an overbroad parent and hit the diagnostic-root bound; that
procedure failure is retained separately.

G2 now has discriminating evidence: the same consumer has Lib.G2.historyCaptured
with an exact Original reference before simplification; afterward it exposes
Val.G1.historyValue0 without that reference. The native association is present
throughout. Do not add a duplicate association path or grant SOURCE rights to
native declarations. See [NATIVE-REUSE-FRONTIER.md](NATIVE-REUSE-FRONTIER.md).

## Remaining work and owners

1. w3_service_owner: finish review/integration of 92fdd75c61, the maintained
   expected-runner-refusal control. Its deliberately retained child stays
   unqualified; parent success proves the normal runner rejects that state.
   Replace runtime compilation of its fixed worker fixture with a declared test
   executable, including the existing daemon/unit consumer. Then execute the
   ordinary configured-source case and the separate outer-refusal controller.
   Do not weaken cleanup checks or broaden native finalization.
2. Root: qualify the joined native-opacity candidate. The typed stage owner,
   plugin and memo now share one policy. The plugin owner adds actual Core,
   interface and reuse controls; w2 adds real session default-method, dictionary,
   specialization and shadowing histories. Preserve constructor/DFun/ClassOp
   representation and imported rules. See NATIVE-REUSE-FRONTIER.md for exact
   design, owner assignments and acceptance; source review is not semantic proof.
3. Independently audit the final joined repairs and purge obligations, then run
   the regional gate. G3/frozen-bundle/Persona acceptance follows later.

The independent 3e6 consumer audit found no unchecked alternate ownership route
in its inspected request/worker/service/settlement scope. This is bounded source
evidence, not a release claim. Its report is
 /srv/swarm/evidence/inanna/domain-astra-tests-20261023/option-c-3e6-opacity-review.md.

The mixed fixture now performs destructive retirement after comparisons requiring
the same compiler realization; all equality and wrong-issuer assertions remain.
The real GHC lifecycle case declares rooted package resources and compares two
independent rooted services instead of the unsupported direct-product path.
The generated-rule regression now rejects omitted loads. The parser positive
uses a complete layout-valid module and passed its actual rerun.

Argon's further dead-code/equality proposals remain recorded investigations,
not implemented changes or prerequisites for this service gate. Dispositions:
 /srv/swarm/evidence/inanna/c-ownership-20261023/audits/PEER-AUDIT-REVIEW.md.

## Evidence and build ownership

Exact commands, revisions and counts:
 /srv/swarm/evidence/inanna/c-ownership-20261022/validation/.
The completed checkpoints are {20f6,923c,4755,3e6c}-checkpoint.json; 4773 logs
are in progress and cannot be aggregated as passing execution yet.
Review, lifetime and trace evidence:
 /srv/swarm/evidence/inanna/c-ownership-20261023/.

Warm checkout tidepool-latent-original-stage-join-20261020 belongs to
validation_resume; the 3e6 pin is released and 47739467ae is now pinned for tests.
Scratch cleanup reclaimed 110.33 GiB; latest free space is about 228.65 GB.
Preserve 100 GiB plus expected growth. Sources, WIP and evidence remain retained.
Integration checkpoints are pushed; main, bundle and Persona are not qualified.
