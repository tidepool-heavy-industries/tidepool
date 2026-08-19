# tidepool-web

The operator GUI for the self-iterating harness, served over HTTP + Datastar
SSE. The model is minimal and harness-agnostic: **a tree of nodes**, where
each node has a starting **seed** prompt, produces a chronological
**timeline** of notes and asks (typed `askUser` forms, the between-loops
continue gate), and ends with a **final value** or a **failure**. The page
is one outline — every node an always-visible, collapsible section, indented
by tree depth, with a derived status badge (`needs you` / `running` /
`done` / `ended` / `failed`).

Stack: axum + maud, Datastar-style SSE-pushed fragments. No JS build step;
the client is one small vendored script inlined into the page (see
`shell.rs`). See `CLAUDE.md` in this crate for the wire contract, the node
lifecycle, and why the gate is sync-blocking.

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

Everything the page does is one of four HTTP verbs — curl-able, no private
API. Interactions are node-scoped and addressed by their own interaction id
(assigned at publish, shown in the `@post` URL baked into each form):

```bash
# The page: every node's section — seed, timeline (notes + pending and
# answered asks), final value/failure, status.
curl -s http://127.0.0.1:4601/

# Live patch stream: one datastar-patch-elements frame per node-scoped
# state change, each replacing that node's #panel-<node_id> in place.
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
`CLAUDE.md`'s "Wire contract" section.

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
