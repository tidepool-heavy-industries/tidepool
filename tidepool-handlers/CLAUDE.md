# tidepool-handlers — concrete effect handlers (per-effect modules)

The Rust side of every `<Eff>Req` — Console, KV, Fs, Http, Exec, Lsp, Llm, Git,
Time, plus the debug-only Meta handler. `build_base_stack`/`base_decls_with_ask`
assemble the fully-wired server. See root `CLAUDE.md` for the project map;
`tidepool-mcp/CLAUDE.md` for the Haskell-facing half of the effect contract
(`*_decl()` + the eval-authoring patterns) — this doc covers the Rust side of
that same contract in more depth.

## Module layout

One module per effect under `src/handlers/`:

- `src/handlers/console.rs` — `ConsoleReq`/`ConsoleHandler`
- `src/handlers/kv.rs` — `KvReq`/`KvHandler` (JSON-file-backed store)
- `src/handlers/fs.rs` — `FsReq`/`FsHandler` + the shared glob/sandbox helpers
  (`expand_glob`, `component_filter`, `pattern_mentions`, `is_glob`, `blake3_hex`)
- `src/handlers/http.rs` — `HttpReq`/`HttpHandler`
- `src/handlers/exec.rs` — `ExecReq`/`ExecHandler`
- `src/handlers/lsp.rs` — `LspReq`/`LspHandler` + the `LspNode`/`LspPosition`/`LspDiag`
  wire types + `json_str`/`json_line`
- `src/handlers/llm.rs` — `LlmReq`/`LlmHandler` + `strictify`, `DEFAULT_OPENAI_MODEL`,
  `LLM_MAX_CALLS`
- `src/handlers/git.rs` — `GitReq`/`GitHandler` + the bridged records
  (`GitCommit`/`GitStatusEntry`/`GitFileDelta`) + `bridged_records_module`
- `src/handlers/time.rs` — `TimeReq`/`TimeHandler`
- `src/handlers/meta.rs` — `MetaReq`/`MetaHandler` (debug path only)

`src/lib.rs` keeps the stack assembly (`HandlerConfig`, `handler_for!`,
`build_base_stack`, `build_minimal_stack`, `base_decls_with_ask`) and
re-exports everything via `pub use handlers::*;` — the external surface is
unchanged from the single-file era (consumers import `tidepool_handlers::FsHandler`
etc. exactly as before). Each module carries its own `#[cfg(test)] mod tests`;
shared test helpers (`full_effect_test_table`, `jit_eval`, `response_value`, …)
live in `src/test_support.rs` (`pub(crate)`, test builds only).

## Adding a new effect constructor here

Each effect module invokes its single-source definition —
`tidepool_mcp::<eff>_effect_def!(crate::effect_glue::effect_rust_projection)`
— which generates the `<Eff>Req` enum (one tuple variant per GADT constructor,
named EXACTLY as in Haskell — no rename layer), `impl DescribeEffect`, and the
`EffectHandler` dispatch whose arms call hand-written inherent methods. What
stays hand-written per module: the handler struct (its fields are
configuration, not contract) and one inherent method per verb —
`fn <method>(&mut self, cx: &EffectContext<'_, CapturedOutput>, <args>) ->
Result<Response, EffectError>`. Adding an operation to an EXISTING effect =
one `verbs` row + helper text in the definition
(`tidepool-mcp/src/effect_defs.rs`) + one inherent method here. A wholly new
effect type needs a new definition, a new module under `src/handlers/`, a
`pub mod` + `pub use` line in `src/handlers/mod.rs`, a `handler_for!` arm in
`src/lib.rs`, and a new positional union-tag slot (see root `CLAUDE.md`'s
locked decision on union tags).

## `cx.respond*` — pick by result shape, not habit

- **`respond(val)`** — the default. One value, converted eagerly via `ToCore`.
  For a TYPED per-verb failure (#335), the errors-tagged method returns
  `Result<T, <ErrEnum>>` and takes no `cx`; the generated dispatch arm wraps it
  with `cx.respond` (`Ok → Right v`, `Err → Left e`), so the handler is total by
  construction — no eval abort for a verb-level failure. (The old
  `respond_caught` substrate for the `try*` zoo is gone: typed failures replace
  it. A genuine panic or a non-`Handler` `EffectError` — real corruption — is
  still the only abort path.)
- **`respond_stream(iter)`** — parks an arbitrary (possibly infinite) Rust
  iterator; the JIT consumes it lazily. Use for open-ended/unbounded sources.
- **`respond_list(vec)`** — an owned `Vec<T>` exposed lazily at ELEMENT
  granularity: list cells materialize eagerly, but each cell's head is a
  thunk that converts its element to a `Value` only when forced (memoized).
  `take 3` converts 3 elements; `length` converts none. Use for a known-size
  collection where callers commonly only need a prefix — the Fs `readGlob` verb
  (`fs_read_glob`, `src/handlers/fs.rs`) is the live call site, exposing
  `[FileRead]` at element granularity.

## Sandboxing

Fs/Exec are rooted at `HandlerConfig.cwd` (the workspace/session sandbox).
Path resolution canonicalizes both the sandbox root and the target path, then
checks `starts_with` — any path resolving outside the root is a loud
`"path escape: ... is outside sandbox"` / `"Path escapes sandbox: ..."`
error, not a silent clamp. This is enforced per-call at the handler, not once
at startup — a symlink or `..` component escaping the sandbox is caught
after canonicalization, not before.

`FsReq::Write` has **mkdir-p semantics**: missing parent directories are
created automatically (`std::fs::create_dir_all`) before writing. Parent
creation is subject to the same sandbox check — the check validates the target
path first, and any ancestor inside the sandbox root is safe by construction.
**Lsp applies the SAME canonicalize+`starts_with` containment** — the daemon's
`resolve.rs::abs_of` canonicalizes the workspace root and the node/file path and
rejects anything resolving outside the root (an untrusted absolute `file` like
`/etc/x.rs` would otherwise replace the root via `Path::join`). That is separate
from `registry::server_for`, which only gates by file extension (`.rs` →
rust-analyzer), not by path. See `tidepool-lsp`'s `CLAUDE.md`.

## Lsp handler specifics

`LspHandler` (`src/handlers/lsp.rs`) is a thin Unix-socket
client to the `tidepool-lsp-daemon` sidecar (`tidepool-lsp` crate — see its
`CLAUDE.md`). No daemon running yields an immediately actionable error:
`"no LSP daemon at <path> — start tidepool-lsp-daemon in the workspace"`
rather than a hang or opaque connection error. `LspNode` (the `Node` wire
type) carries an exact `{name, container, kind, file, pos:{line,char}, text}`
— `pos` is the real UTF-16 position the daemon resolved, so re-addressing a
node doesn't re-search by substring.
