# tidepool-web

The operator GUI for the self-iterating harness, served over HTTP + Datastar
SSE. The model is minimal and harness-agnostic: **a tree of nodes**, where
each node has a starting **seed** prompt, produces a chronological
**timeline** of notes and asks (typed `askUser` forms, the between-loops
continue gate), and ends with a **final value** or a **failure**, with a
derived status (`needs you` / `running` / `done` / `ended` / `failed`).

Two views over that same state:

- **`/`** — a zoomable/pannable **d3 tree**: one status-colored circle per
  node, laid out by `d3.hierarchy`/`d3.tree`. Click a node to open its full
  panel (seed, timeline, pending forms, final value, turn history) in a side
  pane.
- **`/legacy`** — the original outline: every node an always-visible,
  collapsible section, indented by tree depth. Both views update live over
  the same `/sse` stream.

Stack: axum + maud, Datastar-style SSE-pushed fragments. No JS build step;
the client's shared plumbing (`collect`/`post`/`validateRequired`/`toast`/
`applyPatch`) is one small vendored script (`shell::CORE_JS`) embedded by
both pages — see `shell.rs` for `/legacy`'s page-specific glue and `tree.rs`
for the tree view's d3 rendering. d3 v7 itself is vendored as a static asset
(`assets/d3.v7.9.0.min.js`), served locally — no CDN. See `CLAUDE.md` in this
crate for the wire contract, the node lifecycle, and why the gate is
sync-blocking.

Loopback bind only (`127.0.0.1`) — reachability is the authorization
boundary. There is no auth token on the HTTP surface itself; if you're not
on the box, you get in via SSH port-forward / tailnet, not a password.
(`TIDEPOOL_WEB_BIND_HOST` is the one documented, opt-in exception — see
`CLAUDE.md`.)

## Boot

The server binary is `tidepool-selfharness-web`:

```bash
cargo run --bin tidepool-selfharness-web -- --port 4601
```

By itself this only serves the page — nothing drives it without a harness
wired to a `WebGate` (see `tidepool-selfharness.rs`, the harness driver
binary, for how the pieces compose). To see the page working with no
harness, no model, and no API calls:

```bash
cargo run --bin tidepool-selfharness-web -- --demo --port 4601
```

`--demo` runs a mock node tree: the default loop node (note → form →
continue, looping), `root/1-finishes` (a full seed → notes → ask → final
value lifecycle), `root/2-concurrent` (two stacked concurrent asks), and
`root/3-fails` (seed → failure). Open `http://127.0.0.1:4601` and drive it
by hand.

## The verbs

Everything the page does is one of a handful of HTTP verbs — curl-able, no
private API. Interactions are node-scoped and addressed by their own
interaction id (assigned at publish, shown in the `@post` URL baked into each
form):

```bash
# The d3 tree view: full-viewport SVG canvas + side pane, no server-side
# node data (the page's own JS fetches /api/tree and opens /sse).
curl -s http://127.0.0.1:4601/

# The original outline page: every node's section — seed, timeline (notes +
# pending and answered asks), final value/failure, status.
curl -s http://127.0.0.1:4601/legacy

# The tree view's JSON data source: [{id, path, parent, title, status, rev}, ...],
# deriving `status` from the exact same function the outline uses.
curl -s http://127.0.0.1:4601/api/tree

# One node's full panel, standalone — the exact bytes the SSE stream
# patches, and what the tree view's side pane loads on click. A node id
# containing a literal "/" is percent-encoded as a single path segment
# (e.g. root/1-x -> root%2F1-x).
curl -s http://127.0.0.1:4601/node/n1/panel

# The vendored d3 v7.9.0 bundle — served locally, never a CDN.
curl -s http://127.0.0.1:4601/assets/d3.v7.9.0.min.js

# Live patch stream: one datastar-patch-elements frame per node-scoped
# state change, each replacing that node's #panel-<node_id> in place.
# `/legacy` mounts the frame directly; `/` refetches /api/tree on every tick
# and re-joins, applying the SAME frame to the side pane if it's open on
# that node.
curl -s http://127.0.0.1:4601/sse

# Resolve a pending form on node n1. Body is the flat dotted-path object
# the rendered form's inputs bind (rooted at "answer").
curl -sX POST http://127.0.0.1:4601/node/n1/submit/0 \
  -H 'content-type: application/json' \
  -d '{"answer.mood": "calm", "answer.count": 3}'

# Resolve a pending between-loops gate. No body = a bare continue; the
# ContinueSignal sum's flat submission attaches a message.
curl -sX POST http://127.0.0.1:4601/node/n1/continue/1
```

A verb that doesn't match what's pending at that interaction (wrong kind,
unknown node, stale id) returns `{"ok": false, "error": "..."}` with a 400 —
and leaves every pending interaction untouched. Answered asks stay on the
page read-only; re-POSTing their id is rejected as stale.

## Testing

```bash
cargo nextest run -p tidepool-web
```

`tests/operator_gate.rs` boots the real router with `axum::serve` on an
ephemeral port and drives it with a real HTTP client (`reqwest`) — the
`present_form`/`await_continue` round trips across multiple nodes and
stacked asks, the lifecycle fields (seed/final value), timeline persistence
across the loop boundary, error paths, and the SSE stream (initial frames +
a node registered after connect). It asserts only on the wire contract — see
`CLAUDE.md`'s "Wire contract" section. Page-markup fetches in this file
target `/legacy` — the outline's new address.

`tests/tree_view.rs` covers the tree view's own surface the same way: `/`
serves the shell with the vendored d3 route wired, `/api/tree` reports
correct ids/parent links/statuses, `/node/{node}/panel` serves the same
fragment bytes the SSE stream carries (percent-encoded for a slash-path
node id), and `/legacy` still serves the outline.

`tests/form_api.rs` covers the testing-convenience form API the same way.

## Testing convenience: the form API (NOT for browser/production use)

A separate, DISABLED-BY-DEFAULT surface lets a test/agent driver read and
answer a node's pending forms as plain JSON — no browser needed. Off unless
you set:

```bash
TIDEPOOL_FORM_API=1 cargo run --bin tidepool-selfharness-web -- --demo --port 4601
```

```bash
# Every pending form on node n1, each with the interaction id POST must
# echo back.
curl -s http://127.0.0.1:4601/node/n1/api/form

# Resolve one. A missing/wrong/already-resolved interaction id is rejected
# with a 400 and the pending state is left untouched.
curl -sX POST http://127.0.0.1:4601/node/n1/api/form \
  -H 'content-type: application/json' \
  -d '{"interaction": 0, "answer": {"answer.mood": "calm", "answer.count": 3}}'
```

Every response — success or error — carries a `test_only` note. This is a
second front door onto the exact same per-node pending state a browser
`/submit` resolves, not a second, weaker gate: a submission through here is
operator authority, same as through the page. See `src/formapi.rs` for the
full hardening story.
