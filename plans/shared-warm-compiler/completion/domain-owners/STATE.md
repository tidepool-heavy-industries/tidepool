# Domain-owner campaign state

Updated 2026-10-23. G2/G3 and a new Persona bundle remain unqualified.

## Consolidation and build ownership

Integration: `refactor/g2-domain-owners-20261023` in
`/srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020`.
Root source join: `integration/g2-domain-runtime-20261023` in
`/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
The source join feeds integration in coherent tranches; integration is immutable
while the validation worker holds its exact source pin. No new cold build lanes.

## Executed baseline evidence

At `aa93ccab0e`, five focused tests passed: equal-witness acquisition selection;
actual generated publication/recovery history; repeated publication imports;
retained native custody; and historical-custody versus selected-issuer refusal.
The history ran 256 fresh generated cases (258 callbacks), including 842
compositions, 849 recoveries and 2,085 refusal checks, in 381.6 seconds.
Process and compiler cleanup were confirmed where applicable. This establishes
those histories only, not the final refactored bundle.

Exact commands, artifact hashes, full logs and cleanup outcomes:
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/aa93-checkpoint.json`.
The history report retains `diagnostic_evidence_complete=false`; do not replace
that field with a blanket qualification assertion.
A bounded live stack sample found descriptor serialization inside canonical
artifact validation during already-admitted support merges. The P2–P4 owner
removes that reauthentication only for actual admitted rows; raw ingress remains
validated. Its post-change performance and correctness evidence is pending.

## Current source tranches

| Parcels | State and remaining boundary | Owner |
|---|---|---|
| P1 complete products | Draft/certifier and actual Fresh token source complete; integrated in facts group; joined compilation pending | publication_history_repair |
| P2–P4 facts/selection/custody | Actual whole selected rows and separate assembly/admitted views implemented; native callers and OriginalCompilerInputs owner cutover still in progress | astra_context_owner_impl; publication_history_repair |
| P5 Context10 | Recorded codec source ready; authentication/writer integration with actual selected owners in progress; no live Deserialize or equality-as-authority | facts lead / publication_selection_repair |
| P6 native occurrences | Closed HOME/package occurrence and real acquisition source ready in facts group; qualification pending | candidate_guard_resume |
| P7 typed requests | Joined: admission before effects, mandatory SOURCE targets, native/SOURCE exchange separation; actual entrypoint controls awaiting execution | worker_request_shapes |
| P8 interface backing | Captured/Hydrated/Installed owner cutover in progress; actual HMI/NameCache ownership under independent review | interface_backing_impl |
| P9 compiler universe | Joined fixed boot state and common finite/resident kernel; completed-stage ordering source joined; lazy SOURCE backedge control source ready | compiler_backing_lifetime |
| P10 World binding/reservation | Joined paired item reservations and prior-input linking before stage admission; typed decoder repairs joined | world_owner_resume |
| P11 dependency sealing | Joined actual request identity and immutable indexed handles; duplicate stage seal now refused by same ledger transition | astra_dependency_plan_impl |
| P12 ordered program cursor | Joined consuming stage/cursor and exact output issuer; actual controls need execution | compiler_stage_cursor |
| P13 runtime inputs | Existing resolver being folded into one scoped acquired-input owner; exact retained occurrence before image sharing under review | type_consumer_resume |
| P14 execution/capture | Joined actual parked completion owner, closed phases and affine capture ticket; tests and joined compilation pending | type_consumer_resume |
| P15 publication | Joined completed-frontier freeze, mandatory World/lease, exact final selection; completed-only progress and concurrent fence followup source ready | runtime_publication_owner |
| P16 failures | Joined typed decoder/IO/phase causes; exact error assessment repairs included | transition_failure_owner |
| P17 tests | Actual World publication ports integrated incrementally; further recovery, semantic-history and complete-product fixture migrations in flight | validation_resume; fixture workers |

`b9fdda8a8f` joined compilation found incomplete Rust caller migrations, not
executed-test failures: iterator lifetime/indexing, temporary slot borrowing,
new exhaustive failure classification and stale completion test API. The next
source checkpoint repairs these, removes two reconstructed slot/binder fixture
lists, and includes the P14 capture/phase owners. Validation pin `2d23dfdc8b`
compiles toolchain/runtime/extract-cmd first, then the declared source-boot
Haskell test binary only if Rust compilation succeeds. Inspect its retained
report for the terminal outcome; no source-only parcel is a passing test.

## Required next sequence

1. Repair joined compile errors with owning workers; compile tests as well as
   production consumers. Execute the small typed refusal/phase controls early.
2. Fold facts P1–P6 as one owner-consistent slice, including Context10 codec and
   OriginalCompilerInputs; compile before adding more migration layers.
3. Join P8 after independent HMI/NameCache and SOURCE closure review. Validate
   cold/warm/backedge behavior and actual per-binder historical SOURCE carry.
4. Finish P13 single owner, rejecting mismatched runtime imports before physical
   code sharing. No parallel tuple map or identity-based reconstruction.
5. Finish actual World test ports and delete their finite/test-only backdoors.
   Unsupported direct retractions stay explicit refusals, not fabricated
   completed executions. Freeze/retry must not repeat already completed effects.
6. Run the exact joined owner, publication, recovery, historical dependency and
   actor cohorts. Then build/freeze/qualify a matched bundle for G2/G3/Persona.

Keep source-only candidates and evidence distinct. Retain all worker commits;
push consolidated checkpoints. Do not add compatibility routes to get old
fixtures compiling. Shared metadata is generated by its existing owner.
