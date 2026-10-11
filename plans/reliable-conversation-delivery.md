# Reliable conversation and hosted-cell delivery

Accepted implementation baseline: Tidepool `9a5a614661e5db2187d80ed1c488effec0fc3bab`,
Harness `26fb5665d50a38cceabfdc9ec85ac0558a9ab7b8`.

## Outcome

Qualify one matched release that preserves actors after recoverable provider
interruptions, supports validated cross-model captures, repairs native receiver
cancellation and child lookup, and removes repeated hosted-cell preparation.
Preserve the current live run, user notes, credentials, source edits and evidence.
Do not switch the user's server until replacement qualification and coordination.

## Contracts

- The existing Harness Store owns issuance and exact output occurrences. Persist
  issuance before transport and item ownership before early tool admission.
  Content hashes identify bytes, not occurrence identity or authority.
- Item completion, response termination and tool settlement are separate facts.
  Duplicate identical output indices are idempotent; conflicting indices refuse.
  Reconcile buffered events and publish completion atomically.
- One checked projection serves normal requests, context changes, captures,
  model changes, restore, compaction and recovery. Captures freeze exact cuts;
  later response completion cannot enlarge them. Reflection remains available
  even when inference is blocked.
- Cross-model portability uses the existing validated visible-history renderer.
  It does not transfer opaque reasoning or manufacture missing legacy provenance.
- Inference availability is separate from actor lifetime. Input can queue while
  blocked; input, wakes and reconnects cannot authorize recovery.
- Explicit recovery is fenced by run, actor incarnation, head and block
  generation through the existing command journal. Offer qualified exact
  continuation or explicitly selected visible-history continuation with an
  omission receipt. Preserve canonical history and original operation identities;
  never replay effects. Unsettled affected work or uncertain cleanup refuses.
- Existing terminal actors remain terminal. A successor cannot pretend retained
  transcripts restore live runtime values, requests or watches.
- Native cancellation restores the original installed receiver after closing
  its handler realm; it must not synthesize a value for a live continuation.
- Inspection carries the full exact compiler context, including request types.
- Prepared-cell support has one immutable owner with exact per-item selections.
  No new global cache, weakened drift validation or merged historical authority.

## Parallel parcels

| Parcel | Owner |
| --- | --- |
| Issuance, SSE facts, replay, atomic completion | conversation_state_design |
| Checked context, captures and portability | context_projection_design |
| Harness recovery gate and joined contracts | root |
| Tidepool recovery driver, status and browser integration | recovery_delivery_design |
| Native cancellation and real scope controls | bounded_scope_fixture |
| Child lookup exact-context repair | display_prefix_loss |
| Actual hosted-history baseline and comparison | closure_measurement |
| Shared prepared support and per-item selection | finish_display_obligations |
| Typed command errors and existing paging contract | luna_doc_test_jank |
| Compiled prompting/lookup presentation improvements | closure_migration_inventory |
| Prior reviewed parcel integration and counted controls | closure_model_controls |

Workers preserve prior branches, use isolated mounted checkouts, compile affected
consumers early and execute focused tests before returning candidates. Parallel
admission follows actual memory/headroom and checkout output ownership.

## Acceptance

1. Independent shrinking histories cover issuance, repeated bytes, output-index
   gaps, duplicate events, EOF, completion, sync/async settlement, capture,
   recovery, stale actions and reopen. Calibrate with omitted provenance,
   premature continuation and effect-replay mutations.
2. Execute native cancellation positives and refusal/cleanup controls; actual
   child lookup and changed-context refusal; UTF-8 paging, retention and command
   execution exactly once. Do not replace production boundaries with test paths.
3. One matched bundle passes counted M1/M2/M3 plus a scripted provider through
   the actual parser, Store, scheduler, compiler and browser command path:
   root and Luna children; input during sync work; done item/effect followed by
   EOF; blocked but live actors; queued input; explicit recovery without replay;
   context/capture/compaction projection; stale recovery and fatal controls.
4. Replay retained small and Actors histories through the actual host recipe.
   Separate GHC, queue, pre/post-END host work, sealing/reclamation, JIT and
   execution. Report measured changes separately from structural savings.
5. Independently review joined ownership/failure paths, freeze matching assets
   and binaries, then run a real Astra/Luna trial before arranging user delivery.

Detailed historical evidence remains outside source under
`/srv/swarm/evidence/inanna/campaign-closure-20261011/`.
