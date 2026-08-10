# tidepool-web

The minimal operator GUI for the self-iterating harness: one form page served
over HTTP + Datastar SSE. The harness driver has exactly two moments where it
needs a human — filling in a typed form (`askUser`) and clicking "continue"
between loop iterations — and this crate is the web side of both.

Stack: axum + maud, the official Datastar Rust SDK for SSE-pushed fragments.
No JS build step; the Datastar client is one small vendored file inlined into
the page (see `shell.rs`). See `CLAUDE.md` in this crate for the wire
contract, the four verbs, and why the gate is sync-blocking.

Loopback bind only (`127.0.0.1`) — reachability is the authorization
boundary. There is no auth token check on the HTTP surface itself; if you're
not on the box, you get in via SSH port-forward / tailnet, not a password.

## Boot

The server binary is `tidepool-selfharness-web`:

```bash
cargo run --bin tidepool-selfharness-web -- --port 4601
```

- `--port <n>` — defaults to `4601`.

By itself this only serves the page and the `WebGate` — nothing drives it
without a real harness wired to a `WebGate::new(state)` (see
`tidepool-selfharness.rs`, the harness driver binary, for how the pieces
compose). To see the page working with no harness, no model, and no API
calls:

```bash
cargo run --bin tidepool-selfharness-web -- --demo --port 4601
```

`--demo` runs a background mock driver: it presents a representative derived
form, prints the submitted answer to stderr, parks on the continue gate, then
loops. Open
`http://127.0.0.1:4601` and drive it by hand.

## The verbs

Everything the page does is one of four HTTP verbs — curl-able, no private
API:

```bash
# The page shell + whatever's currently pending (a form, the continue
# button, or an idle placeholder).
curl -s http://127.0.0.1:4601/

# Live patch stream: one datastar-patch-elements frame per state change,
# each replacing #panel in place.
curl -s http://127.0.0.1:4601/sse

# Resolve a pending form. Body is a flat { key: scalar } object matching
# the form's fields (enum -> chosen tag string, int -> number, text ->
# string, bool -> bool).
curl -sX POST http://127.0.0.1:4601/submit \
  -H 'content-type: application/json' \
  -d '{"direction": "continue", "iterations": 3, "note": "", "verbose": false}'

# Resolve the between-loops gate. No body.
curl -sX POST http://127.0.0.1:4601/continue
```

A verb that doesn't match what's currently pending (e.g. `/continue` while a
form is pending) returns `{"ok": false, "error": "..."}` with a 400 — and
leaves the pending interaction untouched, so the right verb still works
afterward.

## Testing

```bash
cargo nextest run -p tidepool-web
```

`tests/operator_gate.rs` boots the real router with `axum::serve` on an
ephemeral port and drives it with a real HTTP client (`reqwest`) — the
`present_form`/`await_continue` round trip, both error paths, and the first
`/sse` frame. It asserts only on the wire contract (the `id="panel"` root,
`data-bind`/`data-kind`, `@post` targets, JSON bodies), never on markup —
see `CLAUDE.md`'s "Wire contract" section.

`tests/form_api.rs` covers the testing-convenience form API below the same way.

## Testing convenience: the form API (NOT for browser/production use)

A separate, DISABLED-BY-DEFAULT surface lets a test/agent driver read and
answer the pending form as plain JSON — no browser needed. Off unless you set:

```bash
TIDEPOOL_FORM_API=1 cargo run --bin tidepool-selfharness-web -- --port 4601
```

```bash
# The pending form (or {"pending": false} if nothing is pending), plus a
# nonce that must be echoed back on submit.
curl -s http://127.0.0.1:4601/api/form

# Resolve it. "nonce" must be the value the GET above returned for THIS
# pending occurrence; a missing/wrong/stale nonce is rejected with a 400 and
# the pending form is left untouched.
curl -sX POST http://127.0.0.1:4601/api/form \
  -H 'content-type: application/json' \
  -d '{"nonce": 1, "answer": {"direction": "continue", "iterations": 3, "note": "", "verbose": false}}'
```

Every response — success or error — carries a `test_only` note. This is a
second front door onto the exact same `OperatorGate` a browser `/submit`
resolves, not a second, weaker gate: a submission through here is operator
authority, same as through the page. See `src/formapi.rs` for the full
hardening story (loopback-only inherited from the one server bind, the
nonce/revision tie, disabled-by-default at the router-construction level).
