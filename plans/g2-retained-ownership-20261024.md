# G2 retained ownership implementation

Base: `a3edd881fbc3fa73eb70c03df3423e678313b6e4` (code `dc34094d5e`).
Accepted design: `argon-review-dc34094d5e.md`, SHA256
`a774cd044a2f907f62a10ed72fee32e624c66d313cb0142d99b62261d44678e2`.
The immutable packet and execution evidence are retained under
`/srv/swarm/evidence/inanna/g2-argon-implementation-20261024/`.

## Delivery owners

| Parcel | Owner | Production boundary |
|---|---|---|
| Toolchain provenance | `g2_toolchain_provenance` | Acquisition receipts, exact HOME origins, linked slot witnesses |
| Runtime authorization | `g2_runtime_authorization` | Checked item and admission through slot binding and installation |
| Haskell hydration | `g2_hydrated_owners` | Authenticated decoding, one hydrated allocation, retained completion |
| Stage metadata | `g2_stage_metadata` | Producer sealing and exact reached-group selection |
| Joined histories and integration | root | Production notebook histories, cross-parcel review, obsolete API removal |
| Counted validation | `validation_resume` | Warm validator, exact candidate and declared compiler resources |

Workers use separate branches from the base. Toolchain publishes its runtime
contract early; runtime records that dependency separately. Hydration owns
`ExactHydration`, `ExactScope`, `GhcPipeline`, and the necessary Session capture
issuer. Metadata owns `CompilerProducts` and `CompilerRequest`. Root owns new
joined cases in `typed_segment_tests.rs` and their fixtures/registration.

## Preserved contracts

- Current-stage, prior-program-stage and historical HOME evidence remain
  distinct. Preserve transitive source receipts without relabelling historical
  descendants or silently overwriting conflicting receipts.
- Retained authorization binds the actual item, admission, target and exact
  slot. Keep lexical lease, reachability, live value, representation, signature
  and evaluation checks. The closed path rejects every retained import.
- A stage's incoming inputs do not describe values produced by earlier items
  inside that stage. The receiving compiled item selects those from its sealed
  prefix, retaining the actual producer item, checked row and binder identity.
  Same-stage selection requires earlier item order, the same cell and stage,
  and exact native demand. It grants neither runtime completion nor a lease.
  Cross-stage inputs still require explicit stage selection; historical HOME
  inputs still require their exact terminal. Pooled value availability is never
  a substitute for any of these proofs.
- Accumulated hydrated ownership grants no implicit query selection or lexical
  visibility. Share the same compiler allocation, select exact inputs, and
  preserve failed qualification's environment restoration. Keep mutation local
  to the stage; publish the completed closure only after successful completion.
- Compare evaluated HomeModInfo allocation identities. Independent-allocation
  controls hydrate twice from the original empty-HPT environment sharing its
  NameCache, not from the first hydration's populated environment.
- Seal group metadata once at its producer. Select exact
  `(unit,module,version,ordinal)` coordinates; unrelated versions and unreached
  effect sites do not become selected dependencies.
- Keep the entry compiled and acquired. Authorization deduplication requires
  complete retained-slot coverage, including anchored descendants.

## Acceptance

Compile changed owners and affected test consumers through the pinned admitted
commands. Execute the packet's positive/refusal matrix and new multi-stage
histories with nonzero counted results. Preserve commands, source OIDs, logs,
compiler settlement and diagnostics independently of the test verdict.

Delete superseded paths and test-only alternatives. Integrate complete parcel
deltas, run the joined G2 histories, and return the exact consolidated candidate
to Argon for adversarial review. A component pass or clean review does not
qualify G2/G3, freeze a bundle or authorize a deployment claim.

## Joined frontier after execution

The four ownership parcels are consolidated. The initial joined histories
exposed three adjacent information-loss boundaries; preserve the existing
refusals while repairing their inputs:

| Boundary | Diagnosis | Bounded repair and evidence |
|---|---|---|
| Consuming target selection | The general snapshot join inherits executable roots from earlier targets. A later barrier acquired an unused earlier scaffold and demanded its package interface. | Restrict only the consuming target's execution roots; retain removed exact nodes as custody. Reuse only selected baseline rows at target admission. General joins and complete-prefix validation stay strict. Exercise exact selection, historical custody, changed selected links, and the multi-stage runtime history. |
| Repeated native-demand batches | Later sibling references reacquired a group already issued by an earlier batch; certification had no access to the earlier row. | The request supplies its actual prior acquisitions. Both split-order controls and the real sibling history pass at `794d3efd01`; foreign-operation and changed-dependency refusals remain. No content coalescing or weaker merge equality. |
| Product source membership | Product facts flattened demanded groups into pending payloads. The membership overlay then attempted to reconstruct an exact Original import from a context-free witness. | ProductInputGroups carries the actual Exact or World selected-row owner. Real class-default and transitive histories pass at `85507b78e6`. The direct positive must acquire a real child edge through the request's inventory; nominal metadata alone deliberately grants no execution. |
| Same-stage item values | Item 3 of stage 1 demands `Val.G2.historyRead0`, produced by an earlier item in that same stage. Its stage incoming-input list is correctly empty. | `CellStageRetainedInput` distinguishes stage-selected inputs from sealed earlier-item evidence. Retain producer item and checked row during existing prefix sealing, then consume them only with exact native demand and actual Produced runtime evidence. At `0b4c0cf5d2`, the deterministic cold history completes; its repeat fails later during declaration-import refinement. Direct same-stage and historical controls pass at `35066c2115`. |
| Diagnostic evidence | The metadata history emitted both frontend counters but the displayed prefix omitted the second. Several nearby tests also inferred absence from that prefix. | Selected counters are read completely under explicit observation bounds before independently truncating display. Reader and metadata history pass at `9cd06f6c0a`; four adjacent counter consumers pass at `0b4c0cf5d2`. Remaining raw phase-marker/frontend-text checks are recorded in the evidence audit. |

These are integration findings, not proof that earlier commits introduced
regressions. Source traces and retained failing histories establish the current
mechanisms; unchanged component counts do not qualify these repairs.

The warm validator records each exact candidate under
`/srv/swarm/evidence/inanna/g2-argon-implementation-20261024/`.
The issued-graph overlay positive passes at `a59ac11a36`. Runtime cfg and both
direct runtime ownership controls pass at `35066c2115` (two executed, zero
failed). Two preceding cfg compilations caught test-caller API mistakes and
executed zero tests. Overall diagnostic capture remains incomplete in the final
case reports despite passing assertions and confirmed compiler/process cleanup.
The next review decision is the warm declaration-import boundary, documented in
`g2-argon-mid-effort-review-20261024.md`. Component success does not erase earlier
failures or establish G2. Preserve startup refusals that execute zero tests
separately from actual test outcomes.
