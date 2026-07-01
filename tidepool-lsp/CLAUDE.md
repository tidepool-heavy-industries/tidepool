# tidepool-lsp — `tidepool-lsp-daemon`, the LSP sidecar

A standalone binary (`main.rs`), NOT a library the rest of the workspace
depends on. It spawns and keeps warm one `rust-analyzer` per workspace root,
and serves a tiny newline-delimited-JSON protocol over a Unix socket. The
`LspHandler` that actually implements the `Lsp` effect lives in
`tidepool-handlers` (see its `CLAUDE.md`) as a thin socket client — this
crate is the other end of that socket. See root `CLAUDE.md` for the project
map.

## Daemon architecture

One `rust-analyzer` per running daemon (not per workspace root — nothing
prevents pointing a second daemon at the same root today, though
`registry.rs`'s `server_for` shape leaves room for a real multi-server future
later). Started manually: `tidepool-lsp-daemon [--root DIR] [--socket PATH]`.
Socket resolution: `--socket` flag → `$TIDEPOOL_LSP_SOCK` →
`<root>/.tidepool/lsp.sock`. `LspHandler::new` on the client side resolves the
same way (env var, else `<cwd>/.tidepool/lsp.sock`, no `--socket` equivalent
handler-side) — **the daemon and the handler must agree on `root`/
`TIDEPOOL_LSP_SOCK`, or the handler connects to nothing and returns the
actionable "no LSP daemon at ..." error.** There is no auto-start — if
LSP-backed verbs (`the`/`chart`/`explore` graph verbs, or direct `Lsp*`
effect calls) error, the daemon isn't running yet. rust-analyzer indexing
happens in the background after startup; the daemon serves requests
immediately but they may be slow/incomplete until it logs `ready — workspace
indexed` (falls back to "still indexing after 10min; serving anyway" past a
600s cap — either way requests are never blocked waiting on indexing).

**Protocol deliberately speaks only symbol names and file paths, never raw
LSP positions** — `resolve.rs`'s module docstring: all LSP-shaped detail
(UTF-16 offsets, `WorkspaceEdit`, hover unions, call-hierarchy protocol) is
resolved HERE so the tidepool effect surface never sees it. `where` takes a
bare name (the seed query); every other op takes a whole `Node`
`{name, container, kind, file, pos:{line,char}, text}` and re-resolves it by
reading `pos` back directly — not a substring search, so no wrong-column
aborts on common names.

## Effect surface (declared in `tidepool-mcp`, handled in `tidepool-handlers`)

Ops: `where` (seed by name) → `callers` / `callees` / `references` / `def` /
`hover` / `rename` / `diagnostics` (all node- or file-addressed). **`rename`
returns a unified diff — it does NOT apply the edit.** This crate doesn't
declare or handle the Haskell-facing constructors itself — see
`tidepool-mcp/src/effect_decls.rs` (declaration) and
`tidepool-handlers/src/lib.rs`'s Lsp section (handling); this doc only covers
what happens on the daemon side of the socket call.

## Known limits (verified against current source — some older friction notes are stale, see below)

- **No trait/impl-dispatch operation exists.** The op set above is exhaustive
  — there's no "go to implementations" or trait-dispatch query. If you need
  that, it's not a reliability gap, it's an unimplemented op.
- **`diagnostics` pulls rust-analyzer's own `textDocument/diagnostic`
  analysis, not `cargo check`/`cargo build` output** — these can disagree
  (rust-analyzer's incremental analysis lags or diverges from a real cargo
  invocation). Falls back to rust-analyzer's last-pushed diagnostic cache on
  a pull timeout/error (400ms retry), so a stale-but-present result is
  preferred over a hard failure.
- **No `try`-prefixed LSP op** — a query error (bad node, daemon hiccup)
  propagates as a normal effect error, not a catchable `Either`. Wrap at the
  `tidepool-handlers` `respond_caught` layer if isolation is needed for a
  specific op (not currently wired for Lsp).

**Superseded — do not trust without re-verifying:** an earlier note claimed
"callee re-resolution is brittle because `Node` carries no column." Current
`Node`/`LspNode` DOES carry an exact `pos.char` (0-based UTF-16 column,
`resolve.rs`'s `node()` builder), and re-resolution reads it back directly
rather than searching by substring. If a re-resolution bug resurfaces, it is
NOT this — look elsewhere first.
