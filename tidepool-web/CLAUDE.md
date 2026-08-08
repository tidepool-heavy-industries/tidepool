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
- `bin/tidepool-selfharness-web.rs` — the server binary, plus a `--demo` mock
  driver for opening/reviewing the page with no harness running.
- `bin/tidepool-selfharness.rs` — the actual self-iterating harness driver
  binary (not part of the web surface; lives here because it's this crate's
  other binary target).
- `tests/operator_gate.rs` — the HTTP-level integration test: boots the real
  router with `axum::serve` on an ephemeral port and drives it with a real
  client.

## The four verbs

- `GET /` — the operator page (shell + whatever's currently pending).
- `GET /sse` — Datastar `datastar-patch-elements` frames, one per state
  change, each replacing `#panel` in place. The first frame goes out
  immediately on connect so a page opened mid-interaction is correct without
  waiting for a tick.
- `POST /submit` — resolves a pending form. Body: a flat `{ <key>: <scalar> }`
  JSON object — the canonical `Submission` shape, taken verbatim.
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
empty `Submission` rather than deadlocking the driver — the Haskell-side
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
- The client patches by same-`id` element replacement, and skips replacing an
  element that currently has focus — so a live SSE tick never rips out what
  the operator is mid-typing.

Don't assert on class names, colors, or element nesting anywhere in this
crate's tests — those are `render.rs`/`shell.rs`'s to change freely as long
as the above holds.

## Loopback trust model

Binds `127.0.0.1` only; reachability IS the authorization boundary — there is
no auth token on the HTTP surface itself. Off-box access is via SSH
port-forward or tailnet, not a password. There's no untrusted-input surface
to defend against here the way the old observatory had to worry about
model-supplied `Ui` content: a `FormSpec`'s `label`/`EnumOption::label` text
is server-controlled (the Haskell `askUser` call site), and maud escapes text
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
