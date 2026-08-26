# tidepool-web — selfharness operator UI

## Charter

This crate renders the operator-facing HTTP/SSE interface for the resident
harness: node tree, timeline, typed forms, notes, continue gates, and live
model/effort settings. Harness lifecycle and persistence belong to
`tidepool-harness`.

## State model

The UI is a projection of harness events, not an independent workflow engine.

- The node tree describes ownership and current lifecycle state.
- The timeline is append-only. Never rewrite earlier events to make a later
  state look cleaner.
- Node panels are rendered from server state; browser JavaScript should not
  reconstruct domain state from display strings.
- Unknown or retired nodes return an explicit response rather than falling
  back to another node.

## Operator gate

Form presentation is synchronously blocking by design: the harness waits for a
typed operator answer while the web server continues serving requests. Each
form has an identity; stale or duplicate submissions must not answer a newer
form.

Use the shared `OperatorGate` for forms, notes, continue gates, and steering.
Do not add another input queue or bypass schema decoding.

## Server/browser contract

`render.rs` owns HTML fragments and serialized attributes. `shell.rs` owns the
small browser runtime. Treat IDs, data attributes, endpoint paths, and SSE event
names crossing that boundary as a wire contract.

When changing the contract:

1. update renderer and browser consumer together;
2. test quoting and arbitrary user/model text;
3. keep domain values in structured attributes or JSON, not concatenated JS;
4. preserve stable element identities used by incremental updates.

The tree uses two data paths:

- `/api/tree` for topology and summary state;
- `/node/{node}/panel` for focused detail.

Do not embed complete transcripts in the topology response.

## Forms

`formapi.rs` provides a programmatic test surface for presenting and submitting
typed forms. Keep it behaviorally identical to the browser path: same identity
checks, decoding, errors, and gate settlement.

Generated forms may contain arbitrary constructor and field labels. Escape all
HTML and attribute content. Positional and record payloads must follow the same
schema representation used by `Tidepool.Form`.

## Model settings

The model/effort controls update shared server settings used by subsequent
agent calls. Updates are explicit and observable; they do not mutate an
already-running provider request. Validate values server-side rather than
trusting the select options rendered by the browser.

## Trust boundary

Loopback binding is the default trust model. Binding to a non-loopback address
is an operator choice and must remain explicit. The server has no general web
authentication layer, so do not describe a reachable deployment as secure by
default.

Even on loopback:

- reject malformed paths and form bodies;
- escape all rendered external text;
- do not expose filesystem paths, secrets, or provider credentials in page
  state;
- bound request bodies and accumulated browser-visible history.

## Verification

Use focused renderer, form API, gate, tree endpoint, and settings tests. For
changes to browser behavior, inspect the generated shell and exercise the HTTP
route through the server rather than snapshotting a helper in isolation.

```bash
cargo nextest run -p tidepool-web
```
