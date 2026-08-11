# tidepool-repl — GHCi-style stateful session server

A resident-JIT session surface. One session = one long-lived JIT machine whose
value heap and module scope persist across calls — declarations and bindings
accumulate turn over turn. See the repo-root `CLAUDE.md` for the project map;
`tidepool-mcp/CLAUDE.md` for the shared eval-authoring patterns (Aperture,
`update`/`Edit`/diff verbs) that apply here too.

## The 3 MCP tools + 1 resource

ONE implicit session — the multi-agent story is one repl server per agent, so
there is no session name / no named-sessions map.

- **`session_run { items: [String], input?: Value, verbose?: bool }`** — run a
  block of GHCi-capable items in order; **auto-opens** the session on first use
  (no open step). See Item classification and Response shape below.
- **`session_resume { continuation_id, response }`** — answer an in-turn `ask`
  suspension and run the turn to completion (see Suspension below). A reply that
  doesn't match the suspension's schema is rejected WITHOUT consuming the
  continuation, so it can be retried. A session with a pending suspension will
  not accept a new `session_run` until it is resumed (or the session is reset).
- **`session_reset {}`** — drop the resident machine (freeing the heap and all
  bindings) and open a fresh session. Also drops any pending `ask` continuation:
  **abort folds into reset** (resetting while suspended drops the pending ask).
  The universal get-unstuck button; takes no arguments. Works from a cold start
  (never run) too.

- **`tidepool://session/bindings`** (resource, `application/json`) — read-only
  JSON over LIVE session state: `{bindings: [{name, type, kind (decl|bind),
  generation}], generation, valGeneration}`. Decl-plane heads (`f x = …`,
  `data Foo`, `class C`) are `kind: "decl"`; value/pure binds are `kind: "bind"`.
  Republished after every completed turn; read without driving a turn.

Typical flow: repeated `session_run` → `session_reset` when you want a clean
slate. The `input` field on `session_run` is a payload lane: pass structured
JSON there (e.g. whole-file content for a write) and it's in scope in every item
of that block as `input :: Aeson.Value` — avoids Haskell-string-escaping
large/quote-heavy content in `items` itself.

## Item classification (the block-runner)

Each string in `items` is classified into a kind: **decl** (a top-level
declaration), **stmt** (a bind `x <- e` / `let x = e`, or a bare expression),
or **meta** (a `:command` — `:bindings`, `:reset`, `:t`, `:i`, `:vocab`,
`:browse [Effect]`, `:stub <n>`, `:program`).
Execution stops on the first error. A block ending in a bind leaves the
top-level `value` null (read `items[].result` instead); end with a bare
expression to populate `value`. `:vocab` takes an optional module argument
(`:vocab Diff`) to scope the digest to one module instead of the full blob;
an unknown module name reports clearly rather than returning empty.

**A 4th internal category, `Auto`, backs the decl/stmt split for anything
without a leading keyword.** Only `:`-prefixed items are unambiguously Meta;
items starting with a declaration keyword (`data`/`newtype`/`type`/`class`/
`instance`/`infix*`/`foreign`/`import`/`default`/`{-#`) are unambiguously
Decl. Everything else (including a bare function equation like `f x = x`,
which has no leading keyword) is `Auto`: tried as a Decl first, and on a GHC
parse error, falls back to Stmt. The reported `kind` still comes back as
"decl" or "stmt" — this is invisible from the outside — but it means a
function definition takes a try-then-fallback path, not a direct one.

**decl items compile as their own module.** A signature and its binding —
and all clauses of a multi-clause function — must be in the SAME item. A
genuine multi-clause function is therefore ONE item (`f 0 = ..\nf n = ..`);
its clauses are one declaration and stay together.

**Redefinition across separate items = REPLACE, latest wins (GHCi parity).**
Re-running a decl that reuses a name (`rf x = x+1`, later `rf x = x+2` as two
items) does NOT append an overlapping clause — the newest gen-versioned module
`hiding`s the prior head, so at eval time only the latest `rf` is in scope
(`SessionLib`'s `cumulative_exports_before`). `:program` mirrors this: it emits
only each name's LATEST defining turn (`DeclLog::replayable_sources`, #320), so
the replay is a compilable module, not two conflicting `rf` equations. This is
distinct from a real multi-clause function in one item, which is preserved
whole. (Edge case: a single item co-defining a later-redefined name *and* a
still-live name is kept intact — the live name is faithful, but the stale
co-defined head can duplicate in the flat `:program` text; the documented
one-declaration-per-item idiom avoids this.)

decl and stmt items share the same base import set (Prelude, effect verbs,
`T.`/`Map.`/`Set.`/`L.`/etc., `Aeson`); when a project `Library` facade is on
the include path, both also get `import Library` (guarded by a
`hiding (...)` clause over the session's own cumulative decl heads, so a
decl redefining a name Library also re-exports doesn't become an ambiguous
occurrence). A decl referencing a type from a verb module that `Library`
does NOT re-export still needs its own explicit `import`, same as a stmt
would.

## Response shape (session_run / session_resume)

Default (slim) shape — no generation counters, no double-encoding:

```json
{
  "items": [
    {"kind":"stmt", "ok":true, "bound":"vs", "type":"[Text]"},
    {"kind":"decl", "ok":true, "decl":"slug", "type":"Text -> Text"},
    {"kind":"stmt", "ok":true, "type":"Int"}
  ],
  "value": 42,
  "type": "Int"
}
```

- Each item has `kind` + `ok` + inline result fields (no nested `result` string).
- Bind: `bound` + `type`. Multi-bind: `bound: [names]` + `types: [types]`.
- Decl: `decl` (the declared identifier head: `slug`, `MyData`, `MyClass`, …)
  plus `type` — the GHC-inferred (generalized) type the server had at compile
  time, painted at mutation time so `{decl:"heatOf"}` doesn't cost the caller a
  `:t` round-trip (#317). **Best-effort:** present for VALUE bindings only;
  omitted for `data`/`newtype`/`type`/`class`/`instance`/`import`/fixity decls
  (no term-level type) and when the type probe fails. For a signature+binding
  pair split across two items, only the binding item carries `type`. Painting
  costs one extra extract compile per value decl (the ~6s/turn floor), only
  taken when there's a type to report.
- Non-final expression: `type` (+ `value` for non-last exprs if more items follow).
- Final expression: `type` in the item; `value` and `type` at top-level only.
- Error item: `{"kind":"...", "ok":false, "error":"..."}`.
- Truncated value: `"truncated": "hint"` at top-level alongside `value`.

`verbose: true` — full diagnostic shape for debugging:
`{items:[{index,kind,ok,result:"<JSON string>"}], value, generation, valGeneration}`.
The `result` field is the old double-encoded format. Use this only when you need
generation counters or the raw GHC module name for a declaration.

## Usage notes

- **Default render is `Show`, not `ToJSON`.** A function returning a plain
  ADT (e.g. `checkDiff :: Text -> ParseResult`) renders as derived `Show`
  text; the JSON shape a module's docstring advertises comes from returning an
  `Aeson.Value` (`toJSON <$> ...`) — relevant for the sum-type-returning verbs
  (Diff/Edit/Patch) whose docstrings show JSON.
- **`:vocab` lists modules that are NOT auto-imported.** Only `Library`
  re-exports are in scope bare; other listed verb modules need an explicit
  `import` even though `:vocab` shows them.
- **`grepGlob regex glob`** — content regex FIRST, path glob SECOND (reversed
  order is a common mistake). Regex escaping is quad-backslash (JSON escape ×
  Haskell escape) — e.g. `grepGlob "\\\\.unwrap\\\\(\\\\)" "**/*.rs"`.
- **LSP graph verbs** (`the`/`chart`/`explore`) need `tidepool-lsp-daemon`
  running on the workspace socket; they error cleanly without it.

## Launcher (MCP config)

The MCP client (`~/.claude.json` project section — NOT `.mcp.json`, which is
inert here) launches `~/.cargo/bin/tidepool-repl` directly
(re-`cargo install --path tidepool-repl` to update). The extract is resolved
via `TIDEPOOL_EXTRACT` or, when unset, `tidepool-extract` on PATH — normally
the nix-profile wrapper, which supplies its own with-packages GHC. To test a
working-tree extract change before `scripts/redeploy.sh`, set
`TIDEPOOL_EXTRACT` in the server's `env` block to the cabal-built
`tidepool-extract-bin` and make sure the with-packages GHC is on `PATH`
(the extract shells out to `ghc` and needs `lens` on the DB). This makes the
resolved extract disagree with the last `scripts/redeploy.sh`-written deploy
stamp, which the startup handshake (`tidepool-runtime/src/toolchain.rs`) treats
as skew by default — set `TIDEPOOL_TOOLCHAIN_HANDSHAKE=warn` in the same `env`
block to log the mismatch and continue instead of refusing to start.

## Env knobs

- `TIDEPOOL_PRELUDE_DIR` — override the stdlib dir (falls back to in-repo
  `haskell/lib`). A set-but-invalid value (no `Tidepool/Prelude.hs` under it)
  is a hard startup error.
- `TIDEPOOL_LLM_MODEL` — model for the `llm`/`ask`-adjacent structured calls.
- `TIDEPOOL_TOOLCHAIN_STAMP` — override the deploy-stamp path checked at
  startup (default: under the cache dir, `toolchain-stamp.json`).
- `TIDEPOOL_TOOLCHAIN_HANDSHAKE` — deploy-stamp check severity: `error`
  (default), `warn` (log and continue — the escape hatch for the worktree-extract
  workflow above), or `off`.

## Suspension (`ask`) — what it means for a caller

Hitting the `Ask` effect mid-block suspends the turn: `session_run` returns a
`continuation_id` instead of completing. Nothing is blocked in the OS sense —
the continuation is stowed as data on the session's machine and the rest of the
block is stowed alongside it — but the session accepts no new `session_run`
until you call `session_resume` (to answer and continue the rest of the block)
or `session_reset` (to drop the pending ask and start fresh — abort folds into
reset). A response that doesn't match the suspension's
schema is rejected without consuming the continuation, so a bad `session_resume`
payload can be retried. `session_resume` distinguishes three failure causes
rather than one generic "unknown or expired continuation_id": no session is
running, the session is suspended on a DIFFERENT continuation (names the pending
one), or the session isn't suspended at all.

## Internals: session lifecycle (read if modifying `state.rs`/`manager.rs`/`server.rs`, skip otherwise)

**One engine.** The repl is a single-node client of the same threadless
suspension core `tidepool-harness` drives:
`tidepool_runtime::session::PersistentSession`. An `ask` STOWS — the JIT
continuation stays on the machine as data, the run entry returns `Suspended`,
and the whole `Session` goes back into its slot. No OS thread is parked per
suspended session, and there is no repl-specific ask dispatcher.

Three layers, each owning one thing:

- **`session.rs` — what a turn is.** `run_turn` returns a `TurnStep`
  (`Completed(TurnOutcome)` or `Suspended(AskRequest)`); `resume_turn` /
  `abort_turn` re-enter. Because a suspension outlives the call that made it,
  two things that used to sit on a native stack are plain data on the
  `Session`: the suspended item's `PendingTail` (one variant per run path —
  plain eval, bind, multi-bind, reference, bare-expr `it`) and the block loop's
  `BlockCursor` (results so far, classify verdicts, next index, the pending
  item's index/kind, the `last_*` accumulators, and the `input` payload lane).
  A resume re-enters the machine through the `resume_*` sibling the tail's
  materialization policy calls for and then runs the SAME `finish_*` the
  non-suspending path would have — completion bookkeeping exists once, not
  once per arm. Only a single item can suspend (a decl batch never runs the
  machine, nor does a `:command`), which is why the cursor has one
  pending-item slot rather than a stack.
- **`manager.rs` — where the session is.** `Idle(session) | Running |
  Suspended{session, cont_id}`, with atomic checkout/restore: lock, move the
  session out, run with the lock RELEASED, restore under a second short lock.
  That is `tidepool-harness`'s `SessionRegistry` discipline at N = 1 — itself
  a port of this crate's own `state.rs` discipline. Every checkout carries an
  epoch, so a `session_reset` mid-turn can replace the entry and the in-flight
  turn's restore drops its session instead of clobbering the fresh one.
- **`state.rs` — what the caller is told.** One owned `SessionState` enum
  (Idle/Busy/Suspended/Wedged/Closing) transitioned atomically by the server at
  the dispatch boundary; the ask suspension's caller-facing payload
  (continuation id, expected schema, captured output, TTL clock) lives INSIDE
  `SessionState::Suspended`, not a side map. Read its module docstring before
  changing any of this.

**Load-bearing invariant:** the `SharedState` `parking_lot::Mutex` is NEVER
held across an `.await`. Every transition is lock → inspect/guard → move
owned values out → unlock → then `.await`. Holding it across an await would
deadlock the executor (`parking_lot` is not async-aware).

A turn runs on `tokio::task::spawn_blocking` with the `Session` moved IN and
returned OUT (`Harness::run_checked_out`'s shape), on a big-stack thread inside
that task for deep JIT recursion. One consequence is load-bearing: a runaway
that outruns its abort grace holds the only copy of the session, so `Wedged`
DROPS the whole manager entry rather than pretend it can restore a session it
does not have. The next `session_run` auto-opens a fresh one, and
`session_reset` — which replaces the entry wholesale — is unaffected.

**Timeout/cancel is thread-agnostic and unchanged.** `PauseGate::request_abort`
fires at the next effect dispatch and the JIT `CancelHandle` at the next
GC/tail-call safepoint. The handler stack is wrapped per turn in
`tidepool_runtime::session::GateDispatcher` — the SHARED wrapper, also used by
the eval engine — which makes every effect dispatch a checkpoint and
deliberately does NOT intercept the ask tag: the JIT's own suspend driver
catches that first (`tag >= ask_tag`).

**Effects are handled in `tidepool-handlers/src/lib.rs`**, not
`tidepool-repl/src/main.rs` — main.rs only wires the handler stack via
`build_base_stack` (see the `use tidepool_handlers::{build_base_stack,
HandlerConfig}` import and the `build_base_stack(&hcfg)` call there). Live
stack: Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time (Ask never reaches a
handler — the JIT suspends on its tag; Meta is `--debug`-gated). The stack is
cloned twice over: once per session, and once more per turn, so per-turn
handler state starts clean exactly as it did before the cutover.
