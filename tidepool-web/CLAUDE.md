# tidepool-web — the operator GUI for the self-iterating harness

Built on the **minimal node model**: N registered nodes in a tree
(slash-separated `node_id` paths, convention = wire-carried branch labels),
where each node is one lifecycle — a **seed** prompt in, an append-only
**timeline** of notes and asks, a **final value** (or **failure**) out.
Nothing harness-specific appears in the UI schema — companion drafts,
tensions, and synthesis all flow through those channels as content.

**Two views over that one model**, both live via the same `/sse` stream:

- **`/`** — a zoomable/pannable **d3 tree** ([`tree`] module): one
  status-colored circle per node, laid out by `d3.hierarchy`/`d3.tree`.
  Clicking a node opens its full panel in a side pane.
- **`/legacy`** — the ORIGINAL OUTLINE ([`shell`] module): every node renders
  as its own always-visible section, indented by path depth, sorted
  path-lexicographically with the default node pinned first.

This crate is the web implementation of
`tidepool_harness::selfharness::operator::OperatorGate` — the interface the
driver blocks on when it needs a human, extended by the node-lifecycle
default methods (`node_gate`, `retire_node`, `node_seeded`,
`node_finalized`, `node_failed`).

The SAME `WebGate` also serves `tidepool-harness`'s fork/fanout escalation
ladder (rung 2, a cap-exhausted child awaiting `AllocateMore`/`Abort`) —
`SelfHarnessDriver::set_gate` wires it into `Harness::set_escalation_gate`
automatically, so an escalation presents as an ordinary `present_form` ask
(no dedicated route) and resolves through the existing `/node/{node}/submit/
{interaction}` verb, same as any other pending form.

## What's here

- `server.rs` — axum [`router`], the SSE broadcast stream, [`AppState`] (the
  node registry: per-node seed/timeline/final/failure state), and
  [`WebGate`]: the `OperatorGate` impl, bound to one node. Also the `/`,
  `/legacy`, `/api/tree`, and `/node/{node}/panel` route handlers.
- `render.rs` — [`NodeView`] (borrowing `server::TimelineItem`) → one node's
  `id="panel-<node>"` section: header (full path + collapse toggle + derived
  status badge), collapsed seed `<details>`, the timeline in chronological
  order, failure/final-value blocks, the turn-source history pane. Markup
  and CSS are free to change; the wire contract below is not. Also
  [`render::status`] (the SAME derivation the `/api/tree` JSON reuses) and
  [`render::parent_id`] (slash-path parent derivation, for the tree JSON's
  `parent` field).
- `shell.rs` — `/legacy`'s full HTML document: inline CSS + [`shell::CORE_JS`]
  (the vendored client plumbing — `collect`/`post`/`validateRequired`/
  `toast`/`applyPatch`/`mountPanel` — shared VERBATIM with the tree view) +
  [`shell::JS`] (this page's own SSE-apply glue) + the `#tree` outline of
  stable `.node-slot` wrappers. No CDN, no build step.
- `tree.rs` — `/`'s full HTML document: a d3-hierarchy tree canvas (`#tree-canvas`)
  + a side pane (`#side-pane`), embedding [`shell::CSS`] (the side pane
  renders the SAME node-panel markup as `/legacy`, so it needs those same
  rules) plus [`tree::TREE_CSS`], and [`shell::CORE_JS`] plus this page's own
  [`tree::TREE_JS`] (the d3 rendering: `d3.stratify`/`d3.tree` layout,
  `d3.linkHorizontal` links, `d3.zoom` pan/zoom, a keyed `enter`/`update`/
  `exit` join, refetching `/api/tree` on every `/sse` tick). Also vendors and
  serves d3 v7.9.0 ([`tree::D3_JS`] at [`tree::D3_ASSET_PATH`] —
  `assets/d3.v7.9.0.min.js`, upstream `https://d3js.org`, no CDN at runtime).
- `lib.rs` — `spawn_operator_server_multi(port)`: binds `127.0.0.1:<port>`,
  spawns the axum server on a background task, and returns `(AppState,
  Arc<WebGate>)` — the gate bound to [`DEFAULT_NODE_ID`].
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
  late-registration SSE path. Page-markup fetches target `/legacy` (the
  outline's address since the tree view took over `/`).
- `tests/tree_view.rs` — the tree view's own HTTP-level integration test:
  `/` serves the shell with the vendored d3 route wired, `/api/tree` reports
  correct ids/parent links/statuses (including a pending-ask "needs you"
  case), `/node/{node}/panel` serves the exact fragment bytes the SSE stream
  carries (percent-encoded for a slash-path node id, 404 for an unregistered
  one), and `/legacy` still serves the outline.
- `formapi.rs` + `tests/form_api.rs` — the testing-convenience `GET`/`POST
  /node/{node}/api/form` surface: a second front door onto the SAME per-node
  pending state, never a second gate. See "Testing convenience" below.

## The node lifecycle on this surface

- **Birth** — the driver's eager `node_gate(label)` registers the node (the
  section appears live on any open page — registration pings the SSE tick);
  `node_seeded(label, prompt)` appends a Seeded timeline entry.
- **Life** — `post_note` appends to the timeline; `present_form` appends a
  PENDING ask (the self-iterating harness's between-loops gate is an
  ORDINARY form presented this same way, not a second mechanism);
  `post_turn_source` accumulates the turn history pane.
- **End** — `retire_node(label)` sets `done`; then exactly one of
  `node_finalized(label, value)` / `node_failed(label, reason)` appends the
  outcome as a timeline entry at its true position.
- **Revival** — re-registering a done label (a new agent session under the
  same label) clears `done` and keeps every previous chapter on the timeline.
  This is how the UNIFIED ROOT works: [`DEFAULT_NODE_ID`] is `root`, and the
  driver's default gate and the harness's own root agent session share that
  one node — its timeline interleaves loop narration, each loop iteration's
  root agent session, and the between-turns gate, chapter after chapter.
- **Status is derived at render, never stored**: `needs you` (any pending
  ask — outranks everything, done included: the root is done at every fold
  while its gate is pending) > `running` (not done) > last lifecycle marker
  of the current agent session: `done` (Finalized) / `failed` (Failed) /
  `ended` (neither since the last Seeded).

## The timeline is append-only

Notes and asks land in true chronological order and STAY for the node's
lifetime. Resolving an ask replaces it IN PLACE with its answered form (the
reassembled answer the harness actually received, rendered read-only) — it
never vanishes; notes are never cleared, including across the between-loops
gate's own answered form. The stream above an ask is that ask's context.
Publishing never supersedes
an existing pending ask — concurrent agent sessions each get their own
entry and coexist until answered, in any order. An ask's id is assigned once
from that node's monotonic counter and never reused; POSTing an
already-answered id is rejected as "no such pending interaction".

## The gate is sync-blocking, on purpose

`OperatorGate` (frozen contract, `tidepool-harness/src/selfharness/operator.rs`)
is sync, not async — it mirrors the driver's `block_in_place`/`block_on`
turn-driving. [`WebGate::present_form`] — the ONE gate method, covering the
between-loops gate too — publishes the new ask into `AppState` (pinging the
SSE tick for that node), then parks the calling thread on a
`tokio::sync::oneshot::Receiver::blocking_recv()` resolved from inside an
axum handler. This is why the test files drive gate calls from
`tokio::task::spawn_blocking`. A `WebGate` is bound to exactly one
`node_id` — the ONLY way to mint one is `AppState::register_node(node_id)`,
which is idempotent.

## Wire contract between `render.rs` and `shell.rs`'s JS

The boundary the integration tests assert against; sibling work on
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
  `data-on-submit="@post('/node/<node>/submit/<interaction>')"`.
- The focus-preserving skip is gated on `data-rev`: a matching revision may
  skip replacing a focused panel; a DIFFERENT revision always replaces.

Don't assert on class names, colors, or element nesting anywhere in this
crate's tests — those are `render.rs`/`shell.rs`'s to change freely as long
as the above holds.

## The tree view's two data doors: `/api/tree` and `/node/{node}/panel`

`/` never reads server-rendered node data inline in the page HTML — its own
JS fetches both routes below, same as any other client of this surface
would:

- **`GET /api/tree`** — `[{id, path, parent, title, label, status, rev}, ...]`,
  one entry per registered node, in the same display order the outline uses.
  `id`/`path` are the node id verbatim; `parent` is derived purely from the
  id's slash path ([`render::parent_id`] — `None`/`null` for a root, INCLUDING
  a top-level id registered with no natural parent at all, e.g. a bare `n1`);
  `title` is the truncated display label ([`render::truncate_title`]) the
  outline's `<h2>` also uses; `label` is the tree canvas's OWN, much shorter,
  node text ([`render::tree_label`]) — the node's last path segment only,
  truncated to fit the d3 layout's depth spacing, so a long slug's label can
  never overprint a sibling's or a child's (the tree structure already shows
  ancestry; the full path stays reachable via `path`, drawn into each node's
  native SVG `<title>` hover tooltip, and via the side panel); `status` is the
  SAME class token ([`render::status`]) `node_panel`'s status badge derives —
  the tree view and the outline can never disagree about what "needs you"
  means, because there is exactly one status function. The client's
  `d3.stratify` call synthesizes one invisible super-root parenting every
  null-parent node, so multiple independent top-level ids (a "forest", not
  just one tree) never trip stratify's single-root requirement.

  **Fit-to-content.** [`TREE_JS`] fits the whole tree (root included) into
  the viewport on first render, and again whenever the tree grows past what
  was last fitted (nodes streaming in via SSE) — computed from the laid-out
  tree's own bounding box (`computeBounds`/`fitToContent`), never a hardcoded
  transform. It STOPS auto-fitting the instant a real pan/zoom/wheel gesture
  fires (`d3.zoom`'s `event.sourceEvent`, the standard way to tell an
  operator's own input apart from a programmatic `.call(zoomBehavior.transform,
  ...)`) — a deliberate operator view is never fought. The masthead's
  `#fit-reset` button is the way back: it clears that latch and re-fits.
- **`GET /node/{node}/panel`** — one node's `id="panel-<node_id>"` fragment,
  standalone: the EXACT bytes [`AppState::node_panel_html`] also hands the
  SSE stream. The tree view's side pane loads a clicked node's panel through
  this route on open, then relies on the ordinary `/sse` stream (via the
  shared `applyPatch`) to keep it current afterward — one rendering path, two
  ways to receive it. A node id containing a literal `/` is percent-encoded
  as a single path segment, same discipline as `/node/{node}/submit/{...}`
  (axum matches `{node}` as one segment; a raw slash 404s before any handler
  runs).

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

**The same rule, in d3 terms.** [`tree::TREE_JS`] draws every node label with
`selection.text(...)` — never `.html()`/`.innerHTML` on a live, mounted DOM
node. The one place it parses server HTML text at all (`openPanel`, loading
a node's panel into the side pane) uses the same detached-`<template>`
parse-then-move idiom [`shell::CORE_JS`]'s `applyPatch` already uses for SSE
frames: the fragment is already maud-escaped, server-rendered markup (this
same injection story, applied once, in `render.rs`), so parsing it into DOM
nodes is not a second place that needs to reason about escaping — a NEW
place that assigns fetched/model text as markup on a live element would be.
