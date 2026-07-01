# tidepool-repl — GHCi-style stateful session server

A resident-JIT session surface. One session = one long-lived JIT machine whose
value heap and module scope persist across calls — declarations and bindings
accumulate turn over turn. See the repo-root `CLAUDE.md` for the project map;
`tidepool-mcp/CLAUDE.md` for the shared eval-authoring patterns (Aperture,
`update`/`Edit`/diff verbs, structural search) that apply here too.

## The 5 MCP tools

- **`session_open { session? }`** — spawn a named session worker (default
  name `"default"`; multiple concurrent sessions supported). Call before
  `session_run`.
- **`session_run { items: [String], input?: Value, session? }`** — run a
  block of GHCi-capable items in order; see Item classification below.
- **`session_resume { continuation_id, response }`** / **`session_abort {
  continuation_id }`** — answer or drop an in-turn `ask` suspension (see
  Suspension below). A session with a pending suspension will not accept a
  new `session_run` until one of these is called.
- **`session_close { session? }`** — drop the machine, free the heap.

Typical flow: `session_open` → repeated `session_run` → `session_close`. The
`input` field on `session_run` is a payload lane: pass structured JSON there
(e.g. whole-file content for a write) and it's in scope in every item of that
block as `input :: Aeson.Value` — avoids Haskell-string-escaping large/quote-
heavy content in `items` itself.

## Item classification (the block-runner)

Each string in `items` is classified into a kind: **decl** (a top-level
declaration), **stmt** (a bind `x <- e` / `let x = e`, or a bare expression),
or **meta** (a `:command` — `:bindings`, `:reset`, `:t`, `:i`, `:vocab`).
Execution stops on the first error. A block ending in a bind leaves the
top-level `value` null (read `items[].result` instead); end with a bare
expression to populate `value`.

**A 4th internal category, `Auto`, backs the decl/stmt split for anything
without a leading keyword.** Only `:`-prefixed items are unambiguously Meta;
items starting with a declaration keyword (`data`/`newtype`/`type`/`class`/
`instance`/`infix*`/`foreign`/`import`/`default`/`{-#`) are unambiguously
Decl. Everything else (including a bare function equation like `f x = x`,
which has no leading keyword) is `Auto`: tried as a Decl first, and on a GHC
parse error, falls back to Stmt. The reported `kind` still comes back as
"decl" or "stmt" — this is invisible from the outside — but it means a
function definition takes a try-then-fallback path, not a direct one.

**decl items compile as their own module and have a NARROWER import scope
than stmt items.** A signature and its binding — and all clauses of a
multi-clause function — must be in the SAME item. Statements run inside a
do-block preamble that brings the eval's imports (the session's configured
`imports`, same idea as the sibling stateless `tidepool` eval server) into
scope; a `decl` item does NOT inherit that preamble, so a function signature
referencing an eval-only imported type needs its own explicit `import` even
though `:vocab` lists the type as available.

## Known friction (live, verified against the block-runner surface)

- **Default render is `Show`, not `ToJSON`.** A function returning a plain
  ADT (e.g. `checkDiff :: Text -> ParseResult`) renders as derived `Show`
  text; getting the JSON shape a module's docstring advertises requires
  `toJSON <$> ...` explicitly. Not a bug — "return an `Aeson.Value` to get
  JSON" is the documented rule — but easy to trip on with the newer
  sum-type-returning verbs (Diff/Edit/Patch) whose docstrings show JSON.
- **`:vocab` lists modules that are NOT auto-imported.** Only `Library`
  re-exports are in scope bare; other listed verb modules need an explicit
  `import` even though `:vocab` shows them.
- **`writeFile` does not create parent directories** — fails loud on a
  missing subdirectory rather than `mkdir -p`.
- **`:t` fails on a multi-line type signature with trailing `-- ^` Haddock
  comments per argument** (raw-newline-into-JSON-string bug, reproduced,
  unresolved). Single-line signatures are unaffected; `:i` degrades
  gracefully instead of crashing.
- **`grepGlob regex glob`** — content regex FIRST, path glob SECOND (reversed
  order is a common mistake). Regex escaping is quad-backslash (JSON escape ×
  Haskell escape) — e.g. `grepGlob "\\\\.unwrap\\\\(\\\\)" "**/*.rs"`.
- **LSP graph verbs** (`the`/`chart`/`explore`) need `tidepool-lsp-daemon`
  running on the workspace socket; they error cleanly without it.
- **A trailing `where` on a bare top-level expression breaks the type-probe
  envelope** — the expression runs fine but `{type, value}` reports
  `type: null` (the probe wraps `__t <- <expr>`; a `where` can't attach to a
  single do-statement).
- **Bind names shadowing Prelude** (`head`, `tail`, …) produce an ambiguous-
  occurrence error on reference — use non-colliding bind names.
- **`Match` records** (from `sgFind`) carry the full matched text + every
  metavar; extract only the fields you need rather than returning whole
  matches.

## Env knobs

- `TIDEPOOL_PRELUDE_DIR` — override the stdlib dir (falls back to in-repo
  `haskell/lib`).
- `TIDEPOOL_LLM_MODEL` — model for the `llm`/`ask`-adjacent structured calls.

## Suspension (`ask`) — what it means for a caller

Hitting the `Ask` effect mid-block suspends the turn: `session_run` returns a
`continuation_id` instead of completing. The session is now blocked — no new
`session_run` on it until you call `session_resume` (to answer and continue
the rest of the block) or `session_abort` (to drop it). A response that
doesn't match the suspension's schema is rejected without consuming the
continuation, so a bad `session_resume` payload can be retried.

## Internals: session lifecycle (read if modifying `state.rs`/`server.rs`, skip otherwise)

`state.rs`'s module docstring is the primary source — read it directly before
changing this. Session lifecycle used to be smeared across three disjoint
representations (the `SessionManager` map, the server's `continuations` map,
the worker-local `Option<SessionHandle>`) plus an implicit fourth (which
channel the worker thread is blocked on) — composite states like "Suspended ∧
Closing" had no representation, causing deadlock-on-close-while-suspended,
leak-on-abandon, wedge-on-timeout, and stale-mutation-on-concurrent-run.
Fixed (commit 203548e) by making the lifecycle one owned `SessionState` enum
(Idle/Busy/Suspended/Wedged/Closing), transitioned atomically by the server at
the dispatch boundary; the ask suspension payload lives INSIDE
`SessionState::Suspended`, not a side map.

**Load-bearing invariant:** the `SharedState` `parking_lot::Mutex` is NEVER
held across an `.await`. Every transition is lock → inspect/guard → move
owned values out → unlock → then `.await`. Holding it across an await would
deadlock the executor (`parking_lot` is not async-aware).

`ask.rs`'s worker-thread-parking mechanism (sync `recv`, stack intact)
deliberately duplicates `tidepool-mcp`'s per-eval `ask.rs` against the
resident worker instead of a spawned-per-eval one, rather than widening that
crate's `pub(crate)` visibility (see its module docstring) — `tidepool-mcp`
is left untouched by design. `PauseGate` is the separate timeout-as-yield-
point latch: cancels a runaway turn at the next JIT safepoint rather than
killing the thread.

**Effects are handled in `tidepool-handlers/src/lib.rs`**, not
`tidepool/src/main.rs` (a doc-drift the exploration notes caught — main.rs
only wires the handler stack via `build_base_stack`). Live stack: Console, KV,
Fs, SG, Http, Exec, Lsp, Llm, Ask (Meta is `--debug`-gated).
