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
