# tidepool-web — the minimal operator GUI for the self-iterating harness

A tab strip across N REGISTERED NODES (opaque `node_id` strings, convention =
branch name), each with its own operator interactions. This crate is the web
implementation of `tidepool_harness::selfharness::operator::OperatorGate` —
the seam the self-iterating harness driver blocks on when it needs a human. A
`WebGate` is bound to exactly one registered node; a node can carry SEVERAL
concurrently pending asks at once, all rendered STACKED (an operator gate
never hides a question by superseding an unanswered one).

## What's here

- `server.rs` — axum [`router`], the SSE broadcast stream, [`AppState`] (the
  node registry + per-node ask stacks), and [`WebGate`]: the `OperatorGate`
  impl, bound to one node.
- `render.rs` — a `FormShape` → maud markup for one node's `id="panel-<node>"`
  fragment, stacking every currently pending ask. Markup and CSS are free to
  change; the wire contract below is not.
- `shell.rs` — the full HTML document: inline CSS + the vendored Datastar
  client JS + the tab strip, no CDN, no build step.
- `lib.rs` — `spawn_operator_server_multi(port)`: binds `127.0.0.1:<port>`,
  spawns the axum server on a background task, and returns `(AppState,
  Arc<WebGate>)` — the `AppState` for registering additional nodes
  (`AppState::register_node`), the `WebGate` for the default node
  ([`DEFAULT_NODE_ID`]). `spawn_operator_server(port)` is the single-node
  convenience wrapper (keeps only the default gate) every existing caller
  uses — the one-tab case.
- `bin/tidepool-selfharness-web.rs` — the server binary, plus a `--demo` mock
  driver (TWO nodes: one single-ask/continue loop, one with two CONCURRENT
  asks stacked) for opening/reviewing the page with no harness running.
- `bin/tidepool-selfharness.rs` — the actual self-iterating harness driver
  binary (not part of the web surface; lives here because it's this crate's
  other binary target). Wires `WebGate` into `SelfHarnessDriver::set_gate`
  before `run_loop` unless `--yes`/`--auto`/`--replay` is set (then the
  driver's default headless `StdinGate` stays); also pushes the loaded
  `--harness` source's directory onto the answerer's `EngineConfig::include`
  and fans every driver `Event` out to both a stderr `LogObserver` and a
  durable `JsonlObserver` (`transcript.jsonl`). Single-node: uses
  `spawn_operator_server`, the default-registration convenience.
- `tests/operator_gate.rs` — the HTTP-level integration test: boots the real
  router with `axum::serve` on an ephemeral port and drives it with a real
  client, across multiple nodes and stacked asks.
- `formapi.rs` — the testing-convenience `GET`/`POST /node/{node}/api/form`
  surface: a second front door onto the SAME per-node ask state, never a
  second gate. See "Testing convenience" below.
- `tests/form_api.rs` — the form-api's HTTP-level integration tests, same
  shape as `tests/operator_gate.rs`.

## The verbs

- `GET /` — the operator page: a tab strip across every registered node
  (rendered only when more than one node is registered) plus every node's
  panel, each wrapped in its own stable `.tab-slot`.
- `GET /sse` — Datastar `datastar-patch-elements` frames, one per node-scoped
  state change, each replacing that node's `#panel-<node_id>` in place. An
  initial frame goes out immediately per currently-registered node on
  connect, so a page opened mid-interaction is correct without waiting for a
  tick.
- `POST /node/{node}/submit/{interaction}` — resolves the pending FORM at
  that node/interaction. Body: the renderer's flat dotted-path object,
  validated and reassembled against the pending `FormShape`. `interaction`
  is that ask's own id — assigned once when it's published, never reused —
  which doubles as the ask's `data-rev` and the nonce a stale/wrong id is
  rejected against.
- `POST /node/{node}/continue/{interaction}` — resolves the pending CONTINUE
  gate at that node/interaction. No body required; a plain click.

Both POST verbs return `{"ok": true}` (200) on success or `{"ok": false,
"error": "..."}` (400) on failure. A failure — wrong verb for what's pending
at that interaction, an unknown node, a stale/unknown interaction id,
malformed body — never drops any OTHER pending interaction: a wrong-kind
resolution attempt puts the ask back exactly where it was, at the same
position in that node's stack.

## Concurrency: asks stack, they never supersede

Each registered node holds an ordered stack of pending asks (`Vec`, publish
order), not a single slot. `WebGate::present_form`/`await_continue` APPEND a
new ask onto their node's stack; publishing NEVER replaces or drops an
existing pending ask on that node. This is what lets concurrent cognition
windows (fanout/fork `RunLLMTurn`, `SelfHarnessDriver::set_concurrency_cap`)
each get their own slot and coexist until answered, in any resolution order.
An ask's id is assigned once from that node's monotonic counter and never
reused — resolving it removes it from the stack; POSTing the same id again
afterward is rejected as "no such pending interaction," never silently
resolving a different pending ask.

## The gate is sync-blocking, on purpose

`OperatorGate` (frozen contract, `tidepool-harness/src/selfharness/operator.rs`)
is sync, not async — it mirrors the driver's existing `block_in_place`/
`block_on` turn-driving, which calls it from a blocking context, not an async
task. [`WebGate::present_form`] and [`WebGate::await_continue`] publish the
new ask into `AppState` (pinging the SSE tick for that node), then park the
calling thread on a `tokio::sync::oneshot::Receiver::blocking_recv()`. No
async runtime is entered on the driver's side — the oneshot's `Sender` lives
in `AppState` and is resolved from inside an axum handler (an ordinary async
task) when the matching POST arrives. This is why the test files drive
`present_form`/`await_continue` from `tokio::task::spawn_blocking`: it's the
same shape the real driver uses to call into a sync-blocking gate from an
async context.

A `WebGate` is bound to exactly one `node_id` — the ONLY way to mint one is
`AppState::register_node(node_id)`, which is idempotent (re-registering an
existing id reuses its state) and returns the bound `Arc<WebGate>`. Each
driver still holds an ordinary `Arc<WebGate>` exactly as before; the server
minting one WebGate per registered node is the whole generalization.

## Wire contract between `render.rs` and `shell.rs`'s JS

This is the seam the integration tests assert against, and the one sibling
work on `render.rs`/`shell.rs` must preserve regardless of markup/CSS
changes:

- `node_panel()` always yields exactly one element per node,
  `id="panel-<node_id>"` — the root the SSE stream patches in place for that
  node, and the page shell embeds one per registered node (inside a stable
  `.tab-slot` wrapper — see "Tab switching" below).
- Within a panel, every currently pending ask is STACKED (rendered as its own
  `<form id="ask-<node_id>-<interaction>" data-rev="<interaction>">`) — never
  just the newest.
- Every field input carries `data-bind="<key>"` (the submission key) and
  `data-kind="string|int|number|bool|enum"` (how the client JS coerces its
  value: `int` → `Number`, `bool` → boolean; `string`/`number`/`enum` are all
  left as the raw string value — a `number`-kind (float) field is NOT coerced
  to a JS number today, unlike `int`). A radio group shares one `data-bind`
  key; only the checked option contributes.
- Every action element (submit button, continue button) sits inside a form
  whose `data-on-submit="@post('/node/<node>/submit/<interaction>')"` (or
  `.../continue/<interaction>`) already has the exact node/interaction BAKED
  IN at render time — the vendored JS parses the URL out of that literal
  `@post('...')` string verbatim and needs no node/interaction awareness of
  its own.
- `node_panel()` stamps `data-rev="<n>"` on the panel root, `<n>` being that
  node's aggregate revision (bumped under the SAME lock as every mutation to
  that node's state — F10, now per-node — on every ask publish/resolve or
  notes/turn-history update). The client patches by same-`id` element
  replacement, and skips replacing an element that currently has focus ONLY
  when the incoming `data-rev` matches the currently-mounted element's — a
  DIFFERENT revision (this node's pending state changed) always replaces
  `#panel-<node_id>` regardless of focus. This is what keeps a submit's
  resulting SSE tick from being dropped just because the operator's focus is
  still inside the panel — without the revision gate, that tick would be
  silently skipped and the operator would see stale, already-resolved state.
  Each stacked ask ALSO carries its own `data-rev` (its interaction id,
  stable for its whole pending lifetime) — primarily for identity/nonce
  purposes; the panel replaces wholesale on any change, so per-ask focus
  preservation across a SIBLING ask's arrival/resolution is not attempted.

### Tab switching is a separate, inert concern

Every node's panel is wrapped in a STABLE `.tab-slot` container (never itself
replaced by an SSE patch — only the inner `#panel-<node_id>` is); clicking a
`[data-tab]` button toggles the `.active` class on the matching `.tab-slot`
and tab button. Because the toggle lives on the wrapper rather than the
patched panel, an SSE patch to a hidden node's panel can never resurrect it
into view. A single registered node renders no tab strip at all — the
one-tab case stays visually quiet.

Don't assert on class names, colors, or element nesting anywhere in this
crate's tests — those are `render.rs`/`shell.rs`'s to change freely as long
as the above holds.

## Testing convenience: the form API (`formapi.rs`)

`GET`/`POST /node/{node}/api/form`, mounted alongside the browser verbs by
`router_with_form_api` when explicitly enabled — lets a test/agent driver
read and answer a node's pending forms as plain JSON, no browser needed. This
is NOT a second gate: `GET` reads the SAME per-node ask state the browser
page reads, and `POST` resolves the SAME oneshot a browser
`/node/{node}/submit/{interaction}` would — one state, two front doors.

Hardening is part of the spec, not optional, because a submission through
this door is exactly as much operator authority as one through the page:

- **Disabled by default.** `router()` (used by every existing caller) passes
  `FormApiConfig::default()`, which is `enabled: false`; `merge()` on a
  disabled config returns its input router UNCHANGED — the routes are absent
  from the route table, not merely 404'd behind a flag check at request time.
  The only way to enable it is `TIDEPOOL_FORM_API=1` in the process
  environment, read by `FormApiConfig::from_env()` and wired into
  `spawn_operator_server_multi`'s router build. The env-var parsing itself is
  a pure function (`env_flag_enabled`) so it's unit-tested without mutating
  process env.
- **Loopback only**, inherited rather than reimplemented: these routes mount
  onto the exact `Router` `spawn_operator_server_multi` serves off its one
  hardcoded `127.0.0.1` listener (`lib.rs::bind_addr`). This module never
  opens a socket of its own, so there is no second bind to audit.
- **Per-ask nonce.** `GET /node/{node}/api/form` returns EVERY currently
  pending form on that node, each paired with its own interaction id — the
  same id that's also that ask's `data-rev` and the identifier
  `/submit`/`/continue` resolve by. `POST` requires the exact `interaction`
  id back in `{"interaction": <n>, "answer": {...}}`; a missing, wrong, or
  already-resolved (stale) id is rejected and the pending state is left
  untouched. This is the same trick `shell.rs`'s client JS already uses
  `data-rev` for (telling a genuinely current occurrence apart from a stale
  one) — reused here to require that a caller actually observed the exact
  ask it's answering, not just guessing.
- **Self-describing.** Every response — success or 400 — carries a
  `"test_only"` string naming what this surface is for.

## Loopback trust model

Binds `127.0.0.1` only; reachability IS the authorization boundary — there is
no auth token on the HTTP surface itself. Off-box access is via SSH
port-forward or tailnet, not a password.

**No model-authored content reaches the page.** A form is derived from a
type's own `Generic` metadata, so every label traces back to a Haskell
declaration, and maud escapes text content by construction — including
`node_id` itself, which is always a substrate identifier a caller passed to
`register_node` (convention: a branch name), never model-produced text. That
is why this crate has no injection-surface story of its own — and why
introducing a pane that renders model-produced text (or accepting an
attacker-influenced `node_id`) would need one.
