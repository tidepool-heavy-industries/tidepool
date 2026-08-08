# Wave 2 — `AskUser` form effect + fresh minimal operator GUI

**Status:** active (2026-08-07). Decomposition-ready; exo fan-out.

## Decision context (locked this session)

The **self-iterating harness** (render/loop, `RunLLMTurn`/`Finalize`) is the
target; the older Fork/Dialog interaction-surface plan
(`cool-so-what-s-iterative-bear.md`) is **superseded** — do not execute its
W2/W3/W5. See memory `harness-direction-self-iterating-wins`.

This wave gives the agent/answerer stack its **one non-finalize effect**: the
ability to **spawn a minimal typed form and block for a human answer**. The
old GUI surface (`dialogAsk`/`dialogForm` over the `AskWith` JSON grab-bag,
routed by JSON-key probing) is jank and gets **deleted**, not extended. The
old 7-pane observatory frontend is overdesigned for this — build a **fresh,
focused, award-grade minimal frontend** and lift only the clean transport.

### The four locked calls

1. **`Ask` is repurposed wholesale into the form effect and renamed `AskUser`.**
   Old `Ask` = `ask schema prompt` (structured Q to answerer) + `dialogAsk`/
   `dialogForm` (GUI) riding one `AskWith` constructor. The self-iterating
   agent uses none of that (it continues by writing GHCi, ends with
   `finalize`), so repurposing loses nothing real. New surface:
   `askUser :: Form a -> M a`. Answerer stack: `[AskUser, Finalize]`.
2. **Form primitives, v1: `enum` (1-of-N) / `int` / `text` / `bool` only.**
   `multiChoiceField`/prefill/`prose`/`code` exist and pass wire tests but are
   NOT advertised in v1; add later when a real loop wants them.
3. **The loop is gated on a human clicking a button.** Two operator
   interactions only: *fill a form* (mid-loop `askUser`) and *click continue*
   (advance to the next loop iteration — this REPLACES the between-loops
   stdin-Enter gate at `driver.rs` `between_loops_gate`).
4. **Aesthetic bar: a data-viz poster that wins design awards.** Swiss /
   International Typographic Style — precise grid, generous whitespace,
   hairline rules, ONE restrained accent, semantic boxes, elegant type scale.
   No chrome, no gradients, no decorative shadows. Lines and boxes that carry
   meaning.

## ANTI-PATTERNS (DO NOT)

- DO NOT keep `dialogAsk`, `dialogForm`, the raw `Ui` escape hatch, or the
  `AskWith`-payload `payload.get("ui")` / JSON-key routing. They are deleted.
- DO NOT reuse the 7-pane observatory frontend (`tidepool-web/src/render.rs`
  panes, `shell.rs` 7-pane layout, `bin/tidepool-harness.rs`). Delete the web
  binary in WS5; build fresh in WS3.
- DO NOT add a second event-emission path in the driver — the `Observer` seam
  (`selfharness/observer.rs`) is the extension point; the GUI push is an
  `Observer` (finish `GuiObserver`), not a hardwired call.
- The `OperatorGate` is SYNC-BLOCKING (it mirrors the existing stdin
  `between_loops_gate` and the driver's `block_in_place`/`block_on` turn
  driving — no async-trait). DO NOT spin-wait; block on a channel/oneshot. DO
  call it from a blocking-safe context (`block_in_place`), never on a bare
  async worker, so a park doesn't stall the tokio runtime.
- DO NOT return `Either FormError` from `askUser` — it absorbs failure and
  re-prompts. `Either` stays internal to the decode step.
- DO NOT touch the `Harness`/engine core when deleting the web binary (WS5) —
  only the web frontend goes.

## READ FIRST

- `tidepool-harness/src/selfharness/driver.rs` — `drive_answerer_to_finalize`
  (the `:910` operator-form dead-end this wave fills), `between_loops_gate`
  (:644, the stdin gate → browser button), `service_runllm_hole`, `outer_decls`
  /`answerer_decls`.
- `tidepool-harness/src/engine.rs` — `classify_hole` (:116), `HoleRouting`,
  `SYSTEM_FRAMING` / `ANSWERER_FRAMING`, `json_answer_to_value`.
- `tidepool-mcp/src/effect_defs.rs` — `ask_decl` (:582, the def to rename +
  gut), the single-source `effect_def!` → two-projection scheme (see
  `tidepool-mcp/CLAUDE.md`).
- `haskell/lib/Tidepool/Form.hs`, `Ui.hs` — the applicative builder + wire
  types to trim and re-home under `askUser`.
- `tidepool-web/src/{server.rs,render.rs,shell.rs}` + `bin/tidepool-harness.rs`
  — the transport to LIFT (Axum `/sse`, Datastar patch-apply JS, broadcast
  tick, answer POST) and the panes to DROP.
- `tidepool-harness/src/{log/mod.rs,forcing.rs,effect_trace.rs}` — durable log
  (`TurnStart{source}`, `Event::Effect`, `flush_effects`) for WS4.

## Frozen contracts (root scaffolds + commits FIRST, conflict-free)

These are the cross-workstream seams. Root writes signature-only stubs and
commits before forking, so TLs build against a stable boundary.

1. **`AskUser` effect + canonical wire shape** (`effect_defs.rs`). Rename the
   `Ask` decl to `AskUser`; its ONE constructor carries a **structured form
   spec**, not a JSON grab-bag:
   - Suspension value: `AskUserWith :: FormSpec -> AskUser Value` where
     `FormSpec` is a list of typed fields, each `{ key : Text, label : Text,
     kind : FieldKind }`, `FieldKind = Enum [(Text label, Text tag)] | Int |
     Text | Bool`.
   - Submission: a flat object `{ <key> : <scalar> }` (enum → chosen tag, int →
     number, text → string, bool → bool). ONE shape; no `{values,prose}`
     coercion, no keyed/unkeyed duality.
2. **`OperatorGate` trait** (new, `selfharness/`), the WS2↔WS3 boundary:
   ```rust
   // SYNC-BLOCKING (no async-trait): mirrors the existing stdin gate; the
   // driver calls these from a block_in_place context so a park doesn't stall
   // a tokio worker. A web impl blocks on a channel resolved by an HTTP handler.
   pub trait OperatorGate: Send + Sync {
       fn present_form(&self, spec: &FormSpec) -> Submission; // blocks until submit
       fn await_continue(&self);                              // blocks until the button
   }
   ```
   The driver holds `Arc<dyn OperatorGate>`; the web server (WS3) implements it;
   a headless `StdinGate` keeps the current stdin behavior for non-web runs.
3. **`FormSpec` / `Submission` Rust types** (serde, shared by effect decode,
   gate, SSE frame, and render). Single source in `selfharness/` (or a small
   shared module), re-used everywhere — no per-crate re-derivation.
4. **`Tidepool.Form` surface**: `askUser :: Form a -> M a`; `enumField ::
   Text -> [(Text,a)] -> Form a`, `intField`, `textField`, `boolField`;
   applicative; `askUser` re-prompts on decode failure (the retry lives in
   Haskell, wrapping the raw suspend + `decodeSubmission`).

## Workstreams

### WS1 — `AskUser` effect + trimmed Haskell surface
- Rename/gut `ask_decl` → `askuser_decl`; single structured `AskUserWith`
  constructor per contract 1. Both projections regenerate.
- `classify_hole`: a real constructor arm `AskUserWith → HoleRouting::AskUser
  { spec }` — NO JSON-key probing. Delete the `payload.get("ui")` /
  `HoleRouting::Dialog` / `HoleRouting::Ask` fallback arms and `decode_askwith`.
- `Tidepool.Form`: trim to the four fields + `askUser` (contract 4); delete
  `dialogAsk`/`dialogForm` and the advertised `Tidepool.Ui` escape. Keep the
  `Ui` wire types only as far as render needs (internal).
- Answerer stack `[AskUser, Finalize]`; fix `suspend_tag` / framing references.

### WS2 — Operator servicer (fill the `driver.rs:910` dead-end)
- Add `Arc<dyn OperatorGate>` to the driver. Replace the hard-error: an
  `AskUser` suspension → `gate.present_form(&spec)` (blocking, from a
  `block_in_place` context) → decode via the turn's table → resume the answerer
  with the typed `Value` (mirror `resume_parent`; the decode-retry is
  Haskell-side, so a decode failure re-suspends the same form).
- Replace `between_loops_gate`'s stdin read with `gate.await_continue()`
  (blocking, `block_in_place`).
- Provide `StdinGate` (headless default) so existing tests/CLI keep working.

### WS3 — Fresh minimal operator GUI (`bin/tidepool-selfharness-web.rs`)
- New binary + new slim `render`/`shell` modules. LIFT from `tidepool-web`:
  the Axum `/sse` handler, Datastar patch-apply JS, broadcast tick, the answer
  POST round trip. RENDER only: the pending form (four field kinds) + a
  continue button. No observatory panes.
- Implement `OperatorGate` over the web server: `present_form` publishes the
  spec (SSE frame) + parks a oneshot resolved by `POST /submit`;
  `await_continue` parks a oneshot resolved by `POST /continue`.
- Finish `GuiObserver` (`observer.rs`): push loop `Event`s / the pending-form
  state as Datastar SSE fragments.
- Aesthetic per locked call 4 — this is a design deliverable, not just wiring.

### WS4 — Logging completeness (so "tail the logs" holds)
- Verify the durable per-node log writes `TurnStart{source}` (executed Haskell)
  AND `Event::Effect` (req/resp via `flush_effects`) for the self-iterating
  answerer nodes; confirm `flush_effects` runs on this path. Close any gap.
- Ensure a clean, documented, tailable path under `<cache>/selfharness/`
  (alongside `transcript.jsonl`/`state.json`). Document it in
  `tidepool-harness/CLAUDE.md`.

### WS5 — Delete old surface (light touch)
- Delete `bin/tidepool-harness.rs` (old observatory web binary) + its 7-pane
  `render`/`shell` code. Keep the `Harness`/engine core.
- Drop `dialogAsk`/`Ui` from `ANSWERER_FRAMING`/`SYSTEM_FRAMING`; document the
  `askUser` surface.

## Sequencing & dependencies

- **Root scaffolds the 4 frozen contracts + commits first.**
- **WS1 lands first** (effect + wire; everything else consumes the spec/decode).
- **WS2 → depends on WS1 (routing) + contract 2 (gate).** **WS3 depends on
  contracts 2/3** (implements the gate, renders the spec) — WS2/WS3 parallel
  against the frozen `OperatorGate`.
- **WS4** parallel (touches logging only). **WS5** integrates last.
- Each merge → `cargo check --workspace` gate.

## Verification

- Build/lint: `cargo check --workspace`; `cargo clippy -p tidepool-harness -p
  tidepool-mcp -p tidepool-web`; `cargo fmt --all -- --check`.
- Pure-Rust: `cargo nextest run -p tidepool-harness -p tidepool-mcp`.
- GHC-heavy (real extract): `scripts/battery-shard.sh tidepool-harness -E
  'binary(selfharness_spine)'` stays green; add an `askUser` round-trip
  acceptance (answerer suspends on `askUser (enumField … <*> intField …)`,
  gate submits, typed value resumes, loop reaches next render). Run serially
  (`-j1`) or isolated — these are ~120s each; the sandbox kills bg at ~380s.
- Live smoke: `scripts/redeploy.sh` → drive a real loop; an `askUser` renders
  a clean form in the browser, a submit flows a typed value back, a continue
  click advances the loop; `tail -f <cache>/selfharness/…` shows the executed
  Haskell + effects.

## Out of scope

- `multiChoiceField`/prefill/`prose`/`code` in the advertised surface (later).
- A true separate `Form` union tag (we repurpose `AskUser`; adding a distinct
  tag later forecloses nothing).
- Observatory panes / tree view (deleted; tail the logs instead).
- Non-blocking / timeout form semantics (v1 blocks for the human).
