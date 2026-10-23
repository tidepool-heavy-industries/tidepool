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
| P2–P4 facts/selection/custody | Facts group `e4057b073a` joined, with owner-issued offers and migrated callers. Final single acquired-row map and removal of live equality remain unfinished, pending first joined cfg feedback | astra_context_owner_impl |
| P5 Context10 | Recorded codec, recursive selected-owner authentication and maintained writer index joined. Nested historical-digest discriminator `f16a0f4c7c` joined; execution pending | facts lead / astra_joined_test_review |
| P6 native occurrences | Closed HOME/package occurrence and real acquisition source ready in facts group; qualification pending | candidate_guard_resume |
| P7 typed requests | Joined: admission before effects, mandatory SOURCE targets, native/SOURCE exchange separation; actual entrypoint controls awaiting execution | worker_request_shapes |
| P8 interface backing | Compiler group `471f1f5f9d` joined with actual Captured/Hydrated/Installed owners and independent GHC controls. Review repair `c470cf4d4b` checks package sidecars and nominal requirements before issuer reuse. Joined compilation pending | interface_backing_impl / astra_compiler_cutover_review |
| P9 compiler universe | Joined fixed boot state and common finite/resident kernel; completed-stage ordering source joined; lazy SOURCE backedge control source ready | compiler_backing_lifetime |
| P10 World binding/reservation | Joined paired item reservations and prior-input linking before stage admission; typed decoder repairs joined | world_owner_resume |
| P11 dependency sealing | Joined actual request identity and immutable indexed handles; both focused seal/completion controls passed at `b2ef`; new joined checkpoint still needs validation | astra_dependency_plan_impl |
| P12 ordered program cursor | Joined consuming stage/cursor and exact output issuer; actual controls need execution | compiler_stage_cursor |
| P13 runtime inputs | Single scoped acquired-input owner source checkpoint `7b6fccfd75` awaits matched P14/finite-route join. Exact binding lifetime versus lexical-scope lifetime is under review; do not substitute a scope lease for value custody | type_consumer_resume / astra_runtime_ownership_review |
| P14 execution/capture | Atomic prepared binding/provenance/row/World commit through `1732faefbf`, plus witness-exhaustion preflight `bcdff60584`, joined. Actual issuer, malformed binder, resumed fence and observation controls require execution | astra_runtime_ownership_review |
| P15 publication | Joined completed-frontier freeze, mandatory World/lease, exact final selection; completed-only progress and concurrent fence followup source ready | runtime_publication_owner |
| P16 failures | Joined typed decoder/IO/phase causes; exact error assessment repairs included | transition_failure_owner |
| P17 tests | Actual World publication ports integrated incrementally; further recovery, semantic-history and complete-product fixture migrations in flight | validation_resume; fixture workers |

## Latest executed checkpoints

- `7204`: toolchain/runtime/extract-cmd production and cfg consumers compiled.
- `b2ef`: two dependency seal/completion controls and the actual parked-resume
  owner/revocation control passed.
- `4136078ecac0`: the native source-boot target compiled; four selected tests
  executed and passed (admission ordering, recipe parsing, boot lifetime after
  END, and lazy SOURCE backedge reuse). The final control measures actual
  unchanged and changed warm reuse against independent cold compilation.
  Runtime was 12.58 seconds; total command time including rebuild was 108.4
  seconds. This is the pre-P8 compiler checkpoint, not the newly joined tree.
  Evidence: `/srv/swarm/evidence/inanna/c-ownership-20261022/validation/4136-native-source-boot-focused-four-checkpoint.json`.

The source join now contains facts `e4057b073a`, P14 `1732faefbf`/`bcdff60584`,
and compiler `471f1f5f9d`/`c470cf4d4b`. Conflicts preserve the previously tested
consuming dependency seal, ordinary-source selection, actual loaded HMI reuse,
and package-interface reader fixes. The next warm checkpoint must compile
`tidepool-toolchain`, `tidepool-runtime`, `tidepool-extract-cmd`, and
`tidepool-codegen`, including cfg consumers. No source-only parcel is a test pass.

## Required next sequence

1. Repair joined compile errors with owning workers; compile tests as well as
   production consumers. Execute the small typed refusal/phase controls early.
2. Validate the joined facts P1–P6/Context10 slice before final removal of its
   duplicated live storage and equality surfaces.
3. Validate joined P8 after independent HMI/NameCache and SOURCE closure review:
   cold/warm/backedge behavior and actual per-binder historical SOURCE carry.
4. Finish P13 single owner, rejecting mismatched runtime imports before physical
   code sharing. No parallel tuple map or identity-based reconstruction.
5. Finish actual World test ports and delete their finite/test-only backdoors.
   Production deletion `68f69d6f19` and external ports `0370c8a542`/`cfe7cb3d93`
   remain source-only, not joined here. Preserve the inherited facade execution
   test: its selected ActorSourceImports must reach the production World owner;
   changing it into a metadata-only assertion would discard the contract.
   Unsupported direct retractions stay explicit refusals, not fabricated
   completed executions. Freeze/retry must not repeat already completed effects.
6. Run the exact joined owner, publication, recovery, historical dependency and
   actor cohorts. Then build/freeze/qualify a matched bundle for G2/G3/Persona.

Keep source-only candidates and evidence distinct. Retain all worker commits;
push consolidated checkpoints. Do not add compatibility routes to get old
fixtures compiling. Shared metadata is generated by its existing owner.
