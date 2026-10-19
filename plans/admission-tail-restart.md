# Admission-tail restart handoff

Work is stood down for the user's Codex restart. Do not automatically restart
builds or deploy an older Persona binary. No new resource allowance was admitted.

## Source and ownership

- Coordinator checkout: `/srv/swarm/checkouts/tidepool-admission-tail-integration-20261019`,
  branch `perf/admission-tail-20261019`. Implementation checkpoint
  `bb42160f3cfbdeca3634d20c9519c466f13f7bf0`; this handoff and the baseline evidence
  update are documentation-only successors. Use the branch's exact current OID.
- Pure four-parcel join: `233f988d654db20cc260eb6a7d4f5d22d7295e79`, retained on
  `perf/admission-tail-measured-20261019`. Both branches have GitHub checkpoints.
- Warm execution checkout: `/srv/swarm/checkouts/tidepool-llm-family-effects-20261018`,
  clean detached `20d7ccdcf6ed13a5e0db7ffa89527224a95606ec`. No active owned child.
  Its idle Buck daemon and products remain intact. Do not switch this checkout
  until its execution owner and output/resource state have been reconciled.
- `/srv/swarm/checkouts/tidepool` is unrelated dirty work; do not switch it or
  replace its files. Main/release have not received or qualified this tranche.

## What is complete

The four performance changes are integrated and independently source-reviewed:
scoped parent membership, shared immutable validation facts with separate
per-root permissions, carried verified native closures, and carried admitted
entry Arcs. They remove repeated work through existing production owners.

The entry-Arc issue was duplicate construction hidden by interning, not loss of
custody in the interner. Candidate construction remains. The tests count actual
construction and inspect resource lifetime rather than relying only on pointer
identity. See [the current engine map](../docs/compiler-work-map.md).

Reviewed Console, RuntimeInfo, command/snapshot and two toolchain fixture repairs
are integrated, as are failure-only native-availability diagnostics. Their
joined compilation/execution is still pending; source review is not a test pass.

The instrumented baseline was built, frozen and verified, then passed the full
13-cell packaged history: 1 selected/executed test, 13/13 ordered phases,
404.511 seconds whole-case wall time. This is a scripted-provider generic
prepared/custom history, not authored Persona acceptance. No post-change
comparison exists yet.

Baseline evidence directory:
`/srv/swarm/evidence/inanna/admission-analysis-review-20261018/overnight-20d7-baseline/`.
`BASELINE13-TERMINAL.json` names the reports and their hashes;
`STANDDOWN-HANDOFF.json` retains exact commands, descriptor, resources and process
settlement. Descriptor SHA-256:
`48dc946cd245445b890aa95b64372c6ce41641f19a19e11a75887a8953c2515d`.

## Known unresolved work

| Item | Disposition and next action |
| --- | --- |
| Retained-command group296 compiler refusal | Real production-path failure, no accepted functional repair yet. Run its exact actor test once with newly integrated diagnostics and retain the importing body/selection evidence. Do not weaken Full availability obligations. |
| Optional census test oracle | Independently source-accepted, **not merged**. Donor `/srv/swarm/checkouts/tidepool-optional-census-oracle-20261019`; integrate cumulative `fa1a4390b63521e054c5707f27a1d519c384c1f8` **and** `3a8a5c02090d00230657d178b4c4e06356876736`. The latter fixes an owner-type mistake; do not take the first alone. Compile and execute its counted test. |
| Planned-import fixture repair | **Blocked; do not merge** cumulative `240b0d4a6373952e238db28c4d3db75caa1b093d` / `060bc445e0fd4508c08207786aea73f02d04324f`. Selecting interface roots can retain Original carriers; `for_selected_support` may retain their compiler roles. Custody selection is not namespace demotion. |
| Borrowed-native recapture | Dedicated target still pending. Earlier generic procedure executed zero tests because catalog source retention was stale; that is not a product pass or failure. Use declared `//tidepool/toolchain:toolchain_borrowed_native_recapture_test` resources. |
| Joined qualification and Persona | Four-parcel native controls, matching frozen comparison, and latest actual evolved Persona workspace acceptance remain pending. A generic history pass cannot discharge these gates. |

The earlier c7 focused baseline executed 49 unique cases: 40 passed, 9 failed.
Several now have source repairs, but none may be relabeled passed without a
counted rerun. Its authoritative summary is
`/srv/swarm/evidence/inanna/admission-analysis-review-20261018/overnight-c7-track23/C7-FOCUSED-QUALIFICATION-SUMMARY.json`.

For the blocked fixture, inspect the existing
`CompilerSourceSupport::for_source_owners(&BTreeSet::new())` contract and property
tests as a possible explicit role-demotion route. This is an **unverified lead**.
Do not introduce another test-only authority constructor or relax production
guards. The four local fixture consumers and nine other helper callers have
separate dispositions in
`/srv/swarm/evidence/inanna/admission-analysis-review-20261018/planned-import-owner-projection-20261019/SOURCE-AND-PENDING-GATES.json`.
The independent blocker is beside it in `INDEPENDENT-SOURCE-REVIEW.md`.

## Restart execution order

1. Reconcile the coordinator ledger and exact branches; review and integrate the
   accepted cumulative census repair. Resolve the blocked fixture through a
   reviewed successor, keeping production invariants and assertions intact.
2. Recheck scratch, memory, evidence growth and owned processes. The executor
   proposed NEW28 project growth and a 54 GiB combined evidence guard; these
   were **not admitted** before stand-down. Its last 100 GiB scratch-floor
   headroom was 7,374,139,392 bytes, requiring per-stage recosting.
3. Stage one exact reviewed joined source in the warm checkout. Compile normal
   and test callers, then run the deduplicated counted controls. The current
   roster has 27 core leaves (20 toolchain, 5 host, 2 actor); the optional census,
   planned-import and dedicated recapture gates are additional, not waived.
4. Run the retained-command diagnostic with compiler capture and actual actor
   event logging. Inspect the raw failure before choosing a repair.
5. Freeze the exact accepted joined bundle and repeat the same 13-cell workload
   and resource/settings profile. Compare nested timing intervals without
   double counting. Then qualify the actual Persona workspace before delivery.

Ledger and exact counted selectors/commands:
`/srv/swarm/evidence/inanna/overnight-release-20261018/compiler-perf-eight-integration/`:
`CURRENT-QUALIFICATION-AND-PERF-QUEUE.{json,md}` and
`058E-FINAL-COUNTED-ROSTER.{json,md}`. The latter anchors an earlier source;
regenerate the source-bound roster after any additional merge.

For the group296 test, retain these settings on the ordinary process-group
route, which inherits them (the delegated environment whitelist does not):

```sh
env TIDEPOOL_ACTOR_TEST_TRACE=1 \
  RUST_LOG=tidepool::artifact_inventory::ambiguity_trace=debug \
  TIDEPOOL_KEEP_TEST_LOGS=1 \
  just test-target exomonad-actor actor \
  --exact resident_local_actor::resident_command_retain_binding_returns_reference_without_command_presentation \
  --expected-count 1 --timeout 600 --jobs 1 --output-dir UNIQUE \
  --retain-artifacts --compiler-diagnostics capture --trace-profile full
```

Use the admitted pinned wrapper and protected private configuration from the
executor handoff. Installed `swarm-build` currently accepts `COMMAND [ARG...]`,
not `--disk-growth-gib`; do not repeat that unsupported invocation. Preserve
output/evidence ownership and the existing live reservation. No outputs were
deleted during this stand-down.

## Worker state

All implementation/review workers are completed or stood down. The executor
finished its last owned child successfully and disabled further launches in its
ledger. Delivery and the blocked-fixture owner updated their durable records for
restart; no autonomous follow-up is intended. Resume existing knowledgeable
workers when available; otherwise these source/evidence records own the handoff.
