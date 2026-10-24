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
| Repeated native-demand batches | Later sibling references can reacquire a group already issued by an earlier batch. Source tracing indicates the earlier row is omitted from the new certification inputs. | First confirm which merge comparison fails. Pass the request's actual prior acquisitions through certification, retaining their row identities and ordered links. Do not coalesce equal products or weaken merge identity checks. Cover split and reversed requests plus foreign-owner refusal. |
| Product source membership | Product facts flatten demanded groups into pending payloads. The membership overlay then attempts to reconstruct an exact Original import from a context-free witness. | Preserve the actual selected-row owner in product facts and overlay. Exact carrier-pair and slot checks remain mandatory. No nominal lookup or custody-only permission may replace the missing owner. Review the concrete type before implementation. |

These are integration findings, not proof that earlier commits introduced
regressions. Source traces and retained failing histories establish the current
mechanisms; unchanged component counts do not qualify these repairs.

The warm validator records each exact candidate under
`/srv/swarm/evidence/inanna/g2-argon-implementation-20261024/`.
Its `integrated-025a-checkpoint.json` records 13 distinct executed cases:
eight passes and five failures. One separate compiler-service startup refusal
executed zero tests; its clean settlement and subsequent successful test retry
are retained independently. The current follow-up batch checks repaired test
construction and the bounded target-selection repair before further integration.
