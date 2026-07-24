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

## Defined here during 0b (skeleton compiles before wave 1 forks)

- `SessionNode` / `NodeState` (thunk | running | suspended | waiting-on-
  operator | done | cancelled) — the tree's vocabulary, shared by log,
  forcing, protocol, renderer.
- `HoleId` / `SiteId` newtypes (SiteId = the extract pass's u32).
- `Slot` lifecycle enum for the session registry (segment 20 step 3) —
  lives in tidepool-harness, wraps tidepool-runtime types.
- `ProtocolVerb` request/response serde types (segment 30 C4) + SSE event
  envelope (= log event + monotonic seq).
- `ModelProvider` trait signature (segment 60).
- Crate boundaries: `tidepool-harness` (tree, log, forcing, scheduler,
  provider trait) depends on tidepool-runtime/effect/handlers;
  `tidepool-web` (axum + datastar + maud renderer + observatory) depends
  on tidepool-harness only through the protocol types.
