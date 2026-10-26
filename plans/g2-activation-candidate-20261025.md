# G2 coupled native-offer projection

Integration branch: `integration/g2-domain-runtime-20261023`.
Code candidate: `3215fa40a0c3e4be348ad43d0f53f0247ced43a1`.
Argon review began at `bfeeca3a32`; the follow-up changes two test calls only.
Reviewed starting point: code `f0e599b707`, docs `d861db8beb`.
Current evidence: `/srv/swarm/evidence/inanna/g2-coupled-offers-20261026/`.

G2 remains unqualified. The current work closes native-offer projection and
recorded recovery, preserving exact source, TYPE, native and lifecycle authority.

## Implemented contract

- Non-direct availability intersects each prior contribution with direct-root
  dependencies from the same issuance. Repeated narrowing cannot recreate groups
  from broader offer custody or retain an unrelated standalone Full supplier.
- Explicit source narrowing projects offers and availability together. A sole
  backing supplier may remain as Partial, preserving its exact group and canonical
  Artifact membership without later Full qualification through a TYPE role.
  General support accumulation preserves incoming issued permissions.
- Opaque offer transitions preserve checked rows, exact membership and shared
  contribution owners across Full-to-Partial assembly transitions. No broader
  inventory is used to reconstruct permission.
- PublicationNativeFacts rejects dangling offer references before materialization.
  Its private persistent reference counts follow structural row/contribution
  deltas; removed offers do not trigger a scan of every surviving row. The count
  index is not serialized or an independent authority.
- Demand projection reuses an old cold availability leaf only when its facts are
  unchanged. Changed rows still require the matching IssuedNativePermissionDelta;
  the closing owner maintains their exact new facts. This fixes the joined case
  that combined new offer keys with an old broader row.

The accepted fixture migration now crosses the actual snapshot writer, hydrator
and recorded-row authentication. Controls distinguish private dependency
availability from direct executable root choice. The actor fixtures follow the
real semantic-publication continuation and preserve their adopted scope lease.
No production runtime or Haskell guard was relaxed.

## Current verification

| Source | Executed evidence | Location |
|---|---|---|
| `9c18d4d410` (integrated equivalent `29702745c0`) | Invariant 5/5; removed-offer mutant detected by deterministic and generated histories, saved minimal seed replayed | `invariant/` |
| `65a1c9d29a` | Final focused 20/20: 14 compiler-free and 6 real compiler cases; generator 61/61; generated graph check passed | `projection/` |
| `078c4ca1e8` | Rich publication/private suffix/retraction history 1/1 after demand repair | `joined/final-rich-retraction/` |
| `bfeeca3a32` | Quoted cold/warm, original activation and ready-only generated activation history: 3/3 | `joined/final-*` |
| `3215fa40a0` | Durable activation replacement: 1/1 after correcting two fixture transaction entry points | `joined/final-durable-transaction-fixed/` |

The 20-case component run includes the two previously failing cold restorations,
standalone demotion, true distinct-root narrowing, same-issuance monotonicity,
backing-only Partial warm/codec recovery, complete membership transfer, lifetime
and descendant controls, exact new demand facts, and raw/foreign-delta refusal.
The maintained compiler-free cohort now has 58 registered selectors; registration
is not a claim that all 58 were executed in this round.

The genuine publication fixture has a retained executable wrapper offer that
contributes to the published root. Its independently asserted offer sets therefore
change from three to two when one real root is dropped. A separate issued-cohort
control proves the exact two-to-one case. No Original choice was pruned merely
to make a fixture's mistaken offer count pass. Codec recovery with retained
inventory and physical artifact recovery are recorded separately. No new cyclic
projection execution is claimed.

Independent source reviews accepted the invariant, coupled projection and demand
repair. The first rich run at `41ca891d9d` exposed the old-leaf/new-offer mismatch
missed by source review; its failing evidence remains alongside the passing run.
Early actor failures identified fixture protocol/scope errors. Zero-selection
runner refusals are retained separately and are not counted as tests.

## Next review and qualification

Argon should review the exact integrated candidate, especially dependency-only
Partial retention, narrowing versus accumulation, and the authenticated demand
delta transfer. Retained review receipts and execution pins are in the evidence
directory. The durable replacement fixture follow-up routes its two admission
probes through the existing `with_compiler_world` owner; its focused rerun passed.
Production source is unchanged from the candidate already sent to Argon. The
failing `bfeeca3a32` run remains retained alongside its passing follow-up.

The rich and quoted runs retain complete raw compiler traces and nontruncated
libtest streams, with compiler/process cleanup confirmed. Their bounded diagnostic
summaries truncate samples and five directory resource hashes require their owning
manifest; the reports therefore retain `diagnostic_evidence_complete = false`.
Do not promote these runs to complete release evidence.

One adjacent existing control,
`private_complete_product_prefers_carried_dependency_proof_over_selected_fallback`,
finishes its actual consumer checks but fails a later expectation that a merge
be ambiguous: the merge succeeds. The failure was reproduced using its original
inline issuance caller, so it is not caused by the test-helper factoring. This
boundary remains for separate review; no production choice fallback or changed
ambiguity assertion was introduced. See `fixture-triage/recovery-control/`.

The independent fixture transaction audit found additional positive admission
probes outside a source transaction in tool-installation, preview, provider-owner,
owned-workbench and display fixture paths. These are source-level findings, not
executed failures in this checkpoint. The ready-only generated activation history
explicitly excludes owner-fence cases and does not cover `make_pending`. Preserve
the intentional fenced-owner refusal before private admission; do not add blanket
transactions around all negative controls. Owning paths, selectors and bounded
repair routes are in `review/fixture-transaction-audit.md`. These additional fixture
families require their own repair and execution before wider qualification.

The prior four G2 repairs and bounded cleanup decisions remain accepted at
`f0e599b707`; prior runtime, source-boot and 1,052-program corpus results are under
`/srv/swarm/evidence/inanna/g2-four-boundaries-20261025/` and remain bound to their
recorded revisions. W1 decoder retention and W5 context-baseline redesign remain
deferred; W2's lower-level internal accessor is not globally removed.

Final release acceptance is governed by
[the G2 contract](shared-warm-compiler/completion/g2-completion.md): the exact
frozen bundle, ordinary/uncertain-END histories, Persona startup and bounded
real-provider smoke remain pending. These component checks are not frozen-bundle
qualification or deployment.
