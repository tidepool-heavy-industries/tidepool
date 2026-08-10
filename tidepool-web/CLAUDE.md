# tidepool-web — the minimal operator GUI for the self-iterating harness

One page, two operator interactions. This crate is the web implementation of
`tidepool_harness::selfharness::operator::OperatorGate` — the seam the
self-iterating harness driver blocks on when it needs a human.

## What's here

- `server.rs` — axum [`router`], the SSE broadcast stream, and [`WebGate`]:
  the `OperatorGate` impl.
- `render.rs` — a `FormSpec` → maud markup for the `id="panel"` fragment
  (owned by a sibling workstream; see the frozen-seam note in that file).
- `shell.rs` — the full HTML document: inline CSS + the vendored Datastar
  client JS, no CDN, no build step (also sibling-owned).
- `lib.rs` — `spawn_operator_server(port)`: binds `127.0.0.1:<port>`, spawns
  the axum server on a background task, and returns the `Arc<WebGate>` a
  driver's `set_gate` takes. Both binaries below boot the server through it.
- `bin/tidepool-selfharness-web.rs` — the server binary, plus a `--demo` mock
  driver for opening/reviewing the page with no harness running.
- `bin/tidepool-selfharness.rs` — the actual self-iterating harness driver
  binary (not part of the web surface; lives here because it's this crate's
  other binary target). Wires `WebGate` into `SelfHarnessDriver::set_gate`
  before `run_loop` unless `--yes`/`--auto`/`--replay` is set (then the
  driver's default headless `StdinGate` stays); also pushes the loaded
  `--harness` source's directory onto the answerer's `EngineConfig::include`
  and fans every driver `Event` out to both a stderr `LogObserver` and a
  durable `JsonlObserver` (`transcript.jsonl`).
- `tests/operator_gate.rs` — the HTTP-level integration test: boots the real
  router with `axum::serve` on an ephemeral port and drives it with a real
  client.
- `formapi.rs` — the testing-convenience `GET`/`POST /api/form` surface: a
  second front door onto the SAME [`WebGate`], never a second gate. See
  "Testing convenience" below.
- `tests/form_api.rs` — the form-api's HTTP-level integration tests, same
  shape as `tests/operator_gate.rs`.

## The four verbs

- `GET /` — the operator page (shell + whatever's currently pending).
- `GET /sse` — Datastar `datastar-patch-elements` frames, one per state
  change, each replacing `#panel` in place. The first frame goes out
  immediately on connect so a page opened mid-interaction is correct without
  waiting for a tick.
- `POST /submit` — resolves a pending form. Body: the renderer's flat dotted-
  path object, validated and reassembled against the pending `FormShape`.
- `POST /continue` — resolves the between-loops gate. No body; a plain click.

Both POST handlers return `{"ok": true}` (200) on success or `{"ok": false,
"error": "..."}` (400) on failure. A failure — wrong verb for what's pending,
malformed body — never drops the pending interaction: the handler puts back
whatever it took before erroring out, so the operator can just retry with the
right verb/body.

## The gate is sync-blocking, on purpose

`OperatorGate` (frozen contract, `tidepool-harness/src/selfharness/operator.rs`)
is sync, not async — it mirrors the driver's existing `block_in_place`/
`block_on` turn-driving, which calls it from a blocking context, not an async
task. [`WebGate::present_form`] and [`WebGate::await_continue`] publish the
pending interaction into `AppState` (pinging the SSE tick), then park the
calling thread on a `tokio::sync::oneshot::Receiver::blocking_recv()`. No
async runtime is entered on the driver's side — the oneshot's `Sender` lives
in `AppState` and is resolved from inside an axum handler (an ordinary async
task) when the matching POST arrives. This is why the test file drives
`present_form`/`await_continue` from `tokio::task::spawn_blocking`: it's the
same shape the real driver uses to call into a sync-blocking gate from an
async context.

If a newer interaction supersedes a pending one before it's resolved (the
`oneshot::Sender` gets dropped when `AppState::publish` replaces the pending
slot), `blocking_recv()` returns an error; `present_form` treats that as an
empty JSON object rather than deadlocking the driver — the Haskell-side
decode-retry re-prompts on an empty/invalid submission.

## Wire contract between `render.rs` and `shell.rs`'s JS

This is the seam the integration test asserts against, and the one sibling
work on `render.rs`/`shell.rs` must preserve regardless of markup/CSS
changes:

- `panel()` always yields exactly one element, `id="panel"` — the root the
  SSE stream patches in place and the page shell embeds once.
- Every field input carries `data-bind="<key>"` (the submission key) and
  `data-kind="enum|int|text|bool"` (how the client JS coerces its value:
  int → number, bool → boolean, enum/text → string). A radio group shares one
  `data-bind` key; only the checked option contributes.
- Every action element (submit button, continue button) points at a verb via
  `data-on-submit="@post('/submit')"` or `data-on-click="@post('/continue')"`.
  The vendored JS parses the URL out of that literal `@post('...')` string.
- `panel()` stamps `data-rev="<n>"` on the `#panel` root, `<n>` being
  `AppState`'s revision counter (bumped under the SAME lock as the pending
  slot, on every `publish`/`take`). The client patches by same-`id` element
  replacement, and skips replacing an element that currently has focus ONLY
  when the incoming `data-rev` matches the currently-mounted element's — a
  DIFFERENT revision (a new pending interaction was published) always
  replaces `#panel` regardless of focus. This is what keeps a submit's
  resulting SSE tick from being dropped just because the operator's focus is
  still inside the panel — without the revision gate, that tick would be
  silently skipped and the operator would see a stale, already-resolved form.

Don't assert on class names, colors, or element nesting anywhere in this
crate's tests — those are `render.rs`/`shell.rs`'s to change freely as long
as the above holds.

## Testing convenience: the form API (`formapi.rs`)

`GET`/`POST /api/form`, mounted alongside the four browser verbs by
`router_with_form_api` when explicitly enabled — lets a test/agent driver
read and answer the pending form as plain JSON, no browser needed. This is
NOT a second gate: `GET` reads the SAME `AppState` the browser page reads,
and `POST` resolves the SAME `oneshot::Sender` a browser `/submit` would —
one `WebGate`, two front doors onto it.

Hardening is part of the spec, not optional, because a submission through
this door is exactly as much operator authority as one through the page:

- **Disabled by default.** `router()` (used by every existing caller) passes
  `FormApiConfig::default()`, which is `enabled: false`; `merge()` on a
  disabled config returns its input router UNCHANGED — the routes are absent
  from the route table, not merely 404'd behind a flag check at request time.
  The only way to enable it is `TIDEPOOL_FORM_API=1` in the process
  environment, read by `FormApiConfig::from_env()` and wired into
  `spawn_operator_server`'s router build. The env-var parsing itself is a
  pure function (`env_flag_enabled`) so it's unit-tested without mutating
  process env.
- **Loopback only**, inherited rather than reimplemented: these routes mount
  onto the exact `Router` `spawn_operator_server` serves off its one
  hardcoded `127.0.0.1` listener (`lib.rs::bind_addr`). This module never
  opens a socket of its own, so there is no second bind to audit.
- **Per-prompt nonce.** `AppState::pending_form()` returns the pending
  `FormSpec` alongside `Slot::rev` — the same revision counter F10 already
  bumps exactly once per `publish`/`take`, under the same lock as `pending`.
  `AppState::submit_form(nonce, submission)` only resolves when `nonce`
  matches the CURRENT revision; a missing, wrong, or stale nonce (the form
  was superseded since it was last `GET`) is rejected and the pending form is
  left untouched. This is the same trick `shell.rs`'s client JS already uses
  `data-rev` for (telling a genuinely new pending interaction apart from a
  re-render of the same one) — reused here to require that a caller actually
  observed the exact occurrence it's answering, not just guessing.
- **Self-describing.** Every response — success or 400 — carries a
  `"test_only"` string naming what this surface is for.

## Loopback trust model

Binds `127.0.0.1` only; reachability IS the authorization boundary — there is
no auth token on the HTTP surface itself. Off-box access is via SSH
port-forward or tailnet, not a password. There's no untrusted-input surface
to defend against here the way the old observatory had to worry about
model-supplied `Ui` content: form labels come from Haskell type metadata, and
maud escapes text
content by construction — this crate doesn't need a separate injection-surface
story.

## Superseded

This crate used to serve a 7-pane "observatory" (tree/inspector/transcript/
meters/trace/heap/log) over `tidepool-harness`'s general session-tree engine,
with its own binary (`tidepool-harness`). That surface, its binary, and its
Haskell-side `dialogAsk`/`dialogForm`/`AskWith`-grab-bag counterpart are all
deleted — see `plans/self-iterating-harness/09-askuser-form-gui.md` for why
(the self-iterating harness's `askUser` effect replaced it) and
`plans/README.md` for where that plan sits relative to the superseded
`harness-r0/` plan that originally built the observatory.
