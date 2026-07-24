# Cross-segment contracts (root-owned; finalized during 0b scaffolding)

Segments compile against these so their branches stay conflict-free.
Root (fable) writes the actual Rust/Haskell definitions during crate
scaffolding; leaves treat this file as the index of what is already
decided elsewhere vs defined here.

## Defined in segment specs (do not redefine here)

- `asks.json` sidecar format + `typedSite` payload key —
  `10-extract-pass/SPEC.md` (steps 5–6).
- Event-log jsonl schema (header + event kinds) —
  `30-harness-core/SPEC.md` (C1). Canonical serde types live in
  `tidepool-harness::log`.
- `Ui` ADT + JSON wire shape — `50-ui-edsl/SPEC.md` (fable finalizes at
  E1 spawn; Rust mirror type in `tidepool-harness::ui`).

## Defined (0b landed — crates scaffolded, `cargo check` green)

- `tidepool-harness/src/tree.rs` — `NodeId`, `NodeState` (Thunk/Running/
  Suspended{hole}/Done/Cancelled; waiting-on-operator GLYPH derives from
  hole routing, not a node state), `HoleId` (opaque engine cont-id
  string), `SiteId(u32)`, `FanBadge` (Exact/Bounded/Dynamic),
  `PriceClass` (Zero/Llm/Frontier — draft granularity, segment 30 may
  refine), `Slot<M>` (generic over the machine handle — segment 20
  instantiates; keeps the JIT dependency out of the contract crate).
- `tidepool-harness/src/log.rs` — `LogHeader` (prelude_hash,
  extract_fingerprint, harness_version) + `Event` enum (node_created/
  forced/turn_start/effect{req,resp}/hole_published/hole_answer_attempt/
  hole_consumed/node_done/node_cancelled), `Actor`, `AnswerOutcome`.
  `Effect` carries the RESPONSE — substitution replay depends on it.
- `tidepool-harness/src/provider.rs` — `ModelProvider` trait +
  `TurnRequest`/`TurnResponse`/`Usage`/`ProviderError`.
- Crate boundaries as scaffolded: `tidepool-harness` depends only on
  tidepool-repr + serde (segment 20 adds tidepool-runtime);
  `tidepool-web` depends on tidepool-harness (C4 adds axum/datastar,
  50 adds maud).
- Protocol verb request/response serde types: C4-owned (co-designed with
  the axum handlers); the SSE envelope contract is fixed = `log::Event` +
  a monotonic per-run `seq`.
