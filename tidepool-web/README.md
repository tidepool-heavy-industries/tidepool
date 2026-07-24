# tidepool-web

Protocol server (HTTP+SSE, E1) + the Datastar "observatory" over
`tidepool-harness`. Every capability the observatory's panes use is a
documented, curl-able verb first — the browser UI is just one client. See
`CLAUDE.md` in this crate for the stack (axum + maud + vendored Datastar) and
the F1/F2/F3 freeze this protocol builds against
(`plans/harness-r0/FREEZES.md`).

Loopback bind only (`127.0.0.1`) — reachability is the authorization
boundary. There is no auth token check on the HTTP surface itself; if you're
not on the box, you get in via SSH port-forward / tailnet, not a password.

## Boot

The binary is `tidepool-harness` (in this crate: `src/bin/tidepool-harness.rs`).

```bash
cargo run --bin tidepool-harness -- --port 4600
```

- `--port <n>` — defaults to `4600`.
- `--replay <log.jsonl>` — crash-replay / record-replay mode (below);
  omit for a fresh live run.
- `TIDEPOOL_EXTRACT` — path to the `tidepool-extract-bin` binary (required for
  a live run; see the workspace `CLAUDE.md` for how to build it).
- `TIDEPOOL_PRELUDE_DIR` — override the stdlib include dir (defaults to the
  in-repo `haskell/lib`).
- `TIDEPOOL_LLM_MODEL` — the calling-model id for a fresh OAuth session
  (defaults to `gpt-5`).

Each run writes a fresh, timestamped event log under
`$XDG_STATE_HOME/tidepool/logs` (or `~/.local/state/tidepool/logs`) — a run
never appends to an old log.

## Signing in (ssh -L auth flow)

A fresh (non-`--replay`) run needs a signed-in ChatGPT-subscription OAuth
session before any node can be forced. If the harness runs on a remote box:

```bash
ssh -L 1455:localhost:1455 <harness-box>
```

(or the tailscale equivalent — port `1455` is the OAuth callback port). Then:

```bash
curl -sX POST http://127.0.0.1:4600/auth/start
# => {"authorization_url": "https://...", "port_forward_hint": "..."}
```

Open `authorization_url` in a browser, sign in — the callback lands on
`localhost:1455` via the tunnel above. Poll status:

```bash
curl -s http://127.0.0.1:4600/auth/status
# => {"signed_in": true}
```

The observatory page (`GET /`) shows a "sign in" banner button that drives the
same `/auth/start` verb.

## The observatory panes

`GET /` serves the page shell; `GET /sse` is the live patch stream (Datastar
`datastar-patch-elements` frames). Six panes, each independently scrollable:

- **tree** (`#tree`) — the cognition tree: node id, lifecycle state, fork
  badge, pending-hole prompt teaser. Force/fork buttons post the verbs below.
- **inspector** (`#inspector`) — the first node suspended on an operator
  (`dialogAsk`/`ask`) hole, rendered as a `Ui` form.
- **meters** (`#meters`) — per-node + rollup token usage, folded from
  `TurnDelta` events' `usage` field (assistant turns only).
- **trace** (`#trace`) — per-node effect request/response tail (last 20 per
  node), one collapsed `<details>` per node.
- **heap** (`#heap`) — per-node LIVE heap/GC snapshot, straight off the
  resident `JitEffectMachine` (not folded from the log, unlike meters/trace):
  nursery capacity in bytes, the session heap's bump high-water mark in
  bytes, and the GC-generation count. A node with no live session (thunk,
  done, or cancelled) is simply absent from the table.
- **log** (`#log`) — the last 200 raw log lines (initial render only; not
  SSE-live in R0).

`tree`, `inspector`, `meters`, `trace`, and `heap` all re-render and patch
live over SSE on every logged event (or an internal "tick" nudge right after
a verb mutates the harness).

## Driving via curl

Everything the panes do is one of these verbs. Examples assume `--port 4600`.

**Create + force a root node:**

```bash
curl -sX POST http://127.0.0.1:4600/create \
  -H 'content-type: application/json' \
  -d '{"title": "demo", "prompt": "List the files in the repo root."}'
# => {"ok": true, "node": 0}

curl -sX POST http://127.0.0.1:4600/force/0
# => {"ok": true, "forced": 0}
```

Forcing drives the node's turn loop (in the background) until it completes,
suspends at a hole, or hits the turn cap.

**Answer a suspended hole:**

```bash
# Mechanical option-key answer (D6): the operator picked a Choice option.
curl -sX POST http://127.0.0.1:4600/answer/0/approve

# Free-text / prose answer (routes to the elaboration path):
curl -sX POST http://127.0.0.1:4600/answer/0 \
  -H 'content-type: application/json' \
  -d '{"prose": "Approve, but note the risk in the summary."}'
```

**Drive a fork answerer for a fork hole:**

```bash
curl -sX POST http://127.0.0.1:4600/fork/1
# => {"ok": true, "forking": 1}
```

**Cancel a node:**

```bash
curl -sX POST http://127.0.0.1:4600/cancel/0
```

**Splice an operator message into a node's transcript (F2 `turn_spliced`):**

Interjects `content` into `node`'s OWN transcript, landing at its current
turn position — visible in `node`'s NEXT prompt assembly (the next `force`,
`fork` answerer turn, etc. reads the live transcript this appends to). Logged
as a distinct `turn_spliced` event, not a `turn_delta` — an audit trail can
tell an operator interjection apart from a modeled or harness-generated
turn. Requires `node` to be `running` or `suspended` (same as any other
transcript-mutating verb); a `thunk`/`done`/`cancelled` node has no live
transcript to splice into.

```bash
curl -sX POST http://127.0.0.1:4600/splice/1 \
  -H 'content-type: application/json' \
  -d '{"content": "Operator note: focus on the auth path, ignore the rest."}'
# => {"ok": true}
```

**`eval_in_binding` — a non-consuming heap-browser peek (D4):**

Evaluate a plain `M a` expression against a *suspended* node's live session
heap, without touching its pending hole, the tree state, or the event log —
useful for inspecting in-scope bindings mid-suspension. `name` only labels the
compiled fragment (for diagnostics); it is not a persisted binding — each call
is independent.

```bash
curl -sX POST http://127.0.0.1:4600/eval_in_binding/0 \
  -H 'content-type: application/json' \
  -d '{"name": "peek", "expr": "pure (1 + 1 :: Int)"}'
# => {"ok": true, "name": "peek", "rendered": "2"}
```

Errors (not suspended, compile failure, runtime fault) come back as
`{"ok": false, "error": "..."}` with a 400 status; a compile error is the raw
GHC message, same as everywhere else in the harness.

**Cursor-paged snapshot — for trees too big to fetch in one shot (D1):**

```bash
curl -s 'http://127.0.0.1:4600/snapshot?limit=2'
# => {"ok": true, "nodes": [...], "next_cursor": 1}

curl -s 'http://127.0.0.1:4600/snapshot?cursor=1&limit=2'
# => {"ok": true, "nodes": [...], "next_cursor": null}   # exhausted
```

`cursor` and `limit` are both optional (`limit` defaults to 50, clamped to
500); `next_cursor: null` means the snapshot is exhausted. Each node in
`nodes` is `{node, parent, state, hole, is_fork_hole, hole_prompt}` (`hole` is
the pending hole id when `state` is `"suspended"`, or the reason when
`"cancelled"`; `null` otherwise).

## `--replay` mode

Record-replay / crash-recovery, zero live model calls — the golden-path CI
shape, also usable by hand:

```bash
cargo run --bin tidepool-harness -- --replay ~/.local/state/tidepool/logs/harness-run-171....jsonl
```

This folds the given log to report the terminal tree state (printed to
stderr at boot), then serves a **new** run whose provider replays the prior
run's recorded assistant turns in order (`ReplayProvider`) instead of calling
a live model — force/answer/fork verbs work exactly as in a live run, driving
the replayed turns through the same protocol surface.
