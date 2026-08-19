# tidepool-web — the operator GUI for the self-iterating harness

Built on the **minimal node model**: N registered nodes in a tree
(slash-separated `node_id` paths, convention = wire-carried branch labels),
where each node is one lifecycle — a **seed** prompt in, an append-only
**timeline** of notes and asks, a **final value** (or **failure**) out. The
page is ONE OUTLINE: every node renders as its own always-visible section,
indented by path depth, sorted path-lexicographically with the default node
pinned first. Nothing harness-specific appears in the UI schema — companion
drafts, tensions, and synthesis all flow through those channels as content.

This crate is the web implementation of
`tidepool_harness::selfharness::operator::OperatorGate` — the seam the
driver blocks on when it needs a human, extended by the node-lifecycle
default methods (`node_gate`, `retire_node`, `node_seeded`,
`node_finalized`, `node_failed`).

## What's here

- `server.rs` — axum [`router`], the SSE broadcast stream, [`AppState`] (the
  node registry: per-node seed/timeline/final/failure state), and
  [`WebGate`]: the `OperatorGate` impl, bound to one node.
- `render.rs` — [`NodeView`]/[`TimelineEntry`] → one node's
  `id="panel-<node>"` section: header (full path + collapse toggle + derived
  status badge), collapsed seed `<details>`, the timeline in chronological
  order, failure/final-value blocks, the turn-source history pane. Markup
  and CSS are free to change; the wire contract below is not.
- `shell.rs` — the full HTML document: inline CSS + the vendored client JS +
  the `#tree` outline of stable `.node-slot` wrappers, no CDN, no build step.
- `lib.rs` — `spawn_operator_server_multi(port)`: binds `127.0.0.1:<port>`,
  spawns the axum server on a background task, and returns `(AppState,
  Arc<WebGate>)`; `spawn_operator_server(port)` keeps only the default
  node's gate ([`DEFAULT_NODE_ID`]).
- `bin/tidepool-selfharness-web.rs` — the server binary, plus a `--demo`
  mock node tree (loop node, a full seed→ask→finalize lifecycle, two
  concurrent stacked asks, a failure) for reviewing the page with no
  harness running.
- `bin/tidepool-selfharness.rs` — the actual self-iterating harness driver
  binary (not part of the web surface; lives here because it's this crate's
  other binary target). Wires `WebGate` into `SelfHarnessDriver::set_gate`
  before `run_loop` unless `--yes`/`--auto`/`--replay` is set.
- `tests/operator_gate.rs` — the HTTP-level integration test: boots the real
  router with `axum::serve` on an ephemeral port and drives it with a real
  client, across multiple nodes, stacked asks, the lifecycle fields, and the
  late-registration SSE path.
- `formapi.rs` + `tests/form_api.rs` — the testing-convenience `GET`/`POST
  /node/{node}/api/form` surface: a second front door onto the SAME per-node
  pending state, never a second gate. See "Testing convenience" below.

## The node lifecycle on this surface

- **Birth** — the driver's eager `node_gate(label)` registers the node (the
  section appears live on any open page — registration pings the SSE tick);
  `node_seeded(label, prompt)` stores the authored brief.
- **Life** — `post_note` appends to the timeline; `present_form` /
  `await_continue` append a PENDING ask; `post_turn_source` accumulates the
  turn history pane.
- **End** — `retire_node(label)` sets `done`; then exactly one of
  `node_finalized(label, value)` (the JSON-rendered answer) or
  `node_failed(label, reason)` (the `InvocationExit` rendering).
- **Status is derived at render, never stored**: `failed` > `done`
  (done + value) > `ended` (done, no value) > `needs you` (pending asks) >
  `running`.

## The timeline is append-only

Notes and asks land in true chronological order and STAY for the node's
lifetime. Resolving an ask replaces it IN PLACE with its answered form (the
reassembled answer the harness actually received, rendered read-only) — it
never vanishes; notes are never cleared, including across `await_continue`.
The stream above an ask is that ask's context. Publishing never supersedes
an existing pending ask — concurrent cognition windows each get their own
entry and coexist until answered, in any order. An ask's id is assigned once
from that node's monotonic counter and never reused; POSTing an
already-answered id is rejected as "no such pending interaction".

## The gate is sync-blocking, on purpose

`OperatorGate` (frozen contract, `tidepool-harness/src/selfharness/operator.rs`)
is sync, not async — it mirrors the driver's `block_in_place`/`block_on`
turn-driving. [`WebGate::present_form`] and [`WebGate::await_continue`]
publish the new ask into `AppState` (pinging the SSE tick for that node),
then park the calling thread on a
`tokio::sync::oneshot::Receiver::blocking_recv()` resolved from inside an
axum handler. This is why the test files drive gate calls from
`tokio::task::spawn_blocking`. A `WebGate` is bound to exactly one
`node_id` — the ONLY way to mint one is `AppState::register_node(node_id)`,
which is idempotent.

## Wire contract between `render.rs` and `shell.rs`'s JS

The seam the integration tests assert against; sibling work on
`render.rs`/`shell.rs` must preserve it regardless of markup/CSS changes:

- `node_panel()` always yields exactly one element per node,
  `id="panel-<node_id>"`, stamped with `data-rev` (the node's aggregate
  revision, bumped under the one registry lock on every mutation) and
  `data-path` (the node id — what the client's mount path uses).
- The page shell wraps each panel in a STABLE `.node-slot[data-node-id]`
  wrapper; the SSE patch replaces only the inner panel. Client-side state
  that must survive patches (the collapse toggle's class) lives on the
  wrapper. The default node's wrapper carries `data-pinned`.
- **Mount-on-first-sight**: an SSE frame whose panel id is not in the DOM is
  a node born after page load — the client builds the `.node-slot` wrapper
  itself (indent = `path depth * 14px`, same formula as the server) and
  inserts it into `#tree` at its path-sorted position, pinned slots first.
  Never append to `document.body`.
- Every pending ask renders as its own
  `<form id="ask-<node_id>-<interaction>" data-rev="<interaction>">`; an
  answered ask keeps the same id, no form controls. Every field input
  carries `data-bind="<key>"` and `data-kind="string|int|number|bool|enum"`
  (`int` → `Number`, `bool` → boolean; `string`/`number`/`enum` stay raw
  strings). Action elements bake the exact node/interaction into
  `data-on-submit="@post('/node/<node>/submit/<interaction>')"` (or
  `.../continue/...`).
- The focus-preserving skip is gated on `data-rev`: a matching revision may
  skip replacing a focused panel; a DIFFERENT revision always replaces.

Don't assert on class names, colors, or element nesting anywhere in this
crate's tests — those are `render.rs`/`shell.rs`'s to change freely as long
as the above holds.

## Testing convenience: the form API (`formapi.rs`)

`GET`/`POST /node/{node}/api/form`, mounted alongside the browser verbs by
`router_with_form_api` when explicitly enabled — lets a test/agent driver
read and answer a node's pending forms as plain JSON, no browser needed.
NOT a second gate: one state, two front doors. Hardening is part of the
spec: **disabled by default** (`TIDEPOOL_FORM_API=1` is the only enable, and
a disabled config leaves the routes absent from the route table), **loopback
only** (inherited — it mounts onto the one listener), **per-ask nonce**
(`POST` must echo the exact `interaction` id; stale/wrong ids are rejected
with pending state untouched), **self-describing** (`"test_only"` on every
response).

## Loopback trust model

Binds `127.0.0.1` by default; reachability IS the authorization boundary —
there is no auth token on the HTTP surface itself. Off-box access is via SSH
port-forward or tailnet, not a password.

**`TIDEPOOL_WEB_BIND_HOST` (operator decision, 2026-08-18): a deliberate,
scoped, opt-in exception.** Set it to a specific IP (e.g. this box's
Tailscale interface address) and `bind_addr` binds there instead of loopback.
This does not add auth; it trades the loopback boundary for whatever access
control the target network provides. **Never set it to `0.0.0.0` or a
publicly-routable address** — that would expose an unauthenticated
LLM-harness control surface (form submission = operator authority) to
anything that can route to the box. Falls back to loopback with a loud
stderr warning on an unparseable value.

**Model-authored text renders as escaped text content only.** This page DOES
display model-produced text — seeds, notes, finalized values, turn sources,
failure reasons — and the injection story is exactly one rule: all of it is
interpolated as ordinary maud text nodes (escaped by construction), never as
markup (`PreEscaped`) and never into an attribute value. Form structure is
still derived from a type's own `Generic` metadata, and `node_id` is always
a substrate identifier carried on the wire (never parsed out of a prompt).
A change that renders model text as markup, or accepts model-authored ids,
needs a new injection-surface story first.
