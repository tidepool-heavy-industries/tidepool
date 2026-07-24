# F1/F2/F3 freeze records (S2)

Frozen 2026-07-23 from the shapes the S1 spike actually used (root
review; operator sign-off async — flag objections before widen completes,
after which changes are breaking). Widen leaves build against these
without reopening them. Source of truth for each shape is the CODE cited;
this file records what is frozen and the deliberate seams left open.

## F1 — Ui wire + answer encoding + fragment envelope

- **Ui JSON**: `tidepool-harness/src/ui.rs` serde shape, locked by the
  `wire_shape_is_stable` test (canonical byte-for-byte on the Rust side;
  Haskell `Tidepool.Ui.ToJSON` matches structurally — key ORDER is not
  part of the contract, map-backed emitters sort keys).
- **Answer encoding**: `{"values": {<key>: <value>, …}, "prose": <Text>}`
  — prose channel ALWAYS present (may be empty). Empty prose + known
  option key → MECHANICAL consumption (zero model turns; D6). Non-empty
  prose or unknown shape → routed to the calling model as elaborator,
  show-before-consume (elaboration flow is widen work; the spike stubs it
  pass-through). Prose wins over a conflicting widget.
- **Fragment envelope**: `tidepool-web/src/render.rs` `fragment()` —
  Datastar patch-elements; panes are server-rendered maud, patched over
  SSE. Client runtime is vendored vanilla-JS speaking exactly this wire.

## F2 — log schema + turn events

- **Event enum**: `tidepool-harness/src/log/mod.rs` as merged, including
  the spike's additive turn store:
  - `TurnDelta {node, turn, role, content, usage?}` — `content` is the
    whole message inline in R0; the *delta* framing is the reserved seam
    for streaming (partial content under same `node`+`turn`, no reshape).
    `usage` present on assistant turns only.
  - `TurnForked {node, parent, parent_turn}` — fork = ref to parent
    position; transcript reconstruction folds parent deltas
    `turn <= parent_turn`, then the child's own.
- **Envelope**: `EventRecord {seq, event}` (nested, not flattened);
  `Effect` events carry req AND resp (replay = effect-response
  substitution); header pins `{prelude_hash, extract_fingerprint,
  harness_version}`.
- **Reserved kinds** (names reserved, unbuilt): `turn_spliced` (operator
  interjection into a child conversation), memo events.
- Durability: jsonl, fsync per event, torn-tail-tolerant reader.

## F3 — fork surface

- **Verbs** (all Ask-effect extensions, same ask_tag, NO new union slot):
  `returnControl @T`, `returnControlFork @T` (parks; thunk child;
  transcript forked at checkpoint), `dialogAsk :: Value -> M Value`
  (operator routing via `"ui"` payload field; Ui vocabulary via
  `import Tidepool.Ui`, generated Effects module untouched). Batch
  `returnControlFanout @T :: [Text] -> M [T]` joins in widen under the
  same classification scheme.
- **Classification**: suspension kind is payload-derived from AskWith —
  `typedSite` (extract sidecar site-id → rendered type), `fork` flag,
  `ui` field. asks.json sidecar shape: `[{"site": u32, "type": String}]`,
  always written.
- **Answer semantics**: fork answers are the RAW typed value (the
  monomorphized `resume :: T -> M T` helper pins inference and makes
  mismatches fail with a GHC error naming `T`); ill-typed/bottom answers
  never consume the continuation; GHC error text is the retry prompt,
  verbatim.
- **Child capability**: full parent effect row (same trust domain, §6);
  narrowing = the DispatchEffect allowlist wrapper — designed, ships when
  policy needs it.

## Known-thin (widen backlog, not freeze violations)

- Elaboration fallback stubbed pass-through (F1 path exists, model not
  wired).
- `answer_return_control` built but not test-exercised.
- Harness seed/fork side-maps process-static (one-Harness-per-process R0
  assumption, flagged in code).
- Tree pane snapshot O(n) dense-id scan.
- OAuth live completions: Codex tokens need /v1/responses +
  `chatgpt-account-id` (provider-responses leaf, in flight).
