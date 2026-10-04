# tidepool-handlers — concrete effect handlers (per-effect modules)

**Charter.** Belongs: the concrete Rust `<Eff>Req` handler implementations
(Console/KV/FsRead/FsWrite/Http/Exec/Llm/Git/Time/Meta/Event/Worktree) and stack
assembly (`build_base_stack`). Does NOT belong: effect/verb type definitions
(`tidepool-mcp`'s `effect_defs.rs` / `tidepool-protocol`'s schema), the git
primitives a `WorktreeHandler` call wraps (`exomonad-worktree`).

The Rust side of every `<Eff>Req` — Console, KV, FsRead, FsWrite, Http, Exec, Llm, Git,
Time, plus the debug-only Meta handler. `build_base_stack`/`base_decls`
assemble the fully-wired server. See root `CLAUDE.md` for the project map;
`bridge/mcp/CLAUDE.md` for the Haskell-facing half of the effect contract
(`*_decl()` + the eval-authoring patterns) — this doc covers the Rust side of
that same contract in more depth.

## Module layout

One module per effect under `src/handlers/`:

- `src/handlers/console.rs` — `ConsoleReq`/`ConsoleHandler`
- `src/handlers/kv.rs` — `KvReq`/`KvHandler` (JSON-file-backed store)
- `src/handlers/fs.rs` — the shared `FsBackend`, `FsReadHandler`, and
  `FsWriteHandler`, plus glob/sandbox helpers
  (`expand_glob`, `component_filter`, `pattern_mentions`, `is_glob`, `blake3_hex`)
- `src/handlers/http.rs` — `HttpReq`/`HttpHandler`
- `src/handlers/exec.rs` — `ExecReq`/`ExecHandler`
- `src/handlers/llm.rs` — `LlmReq`/`LlmHandler` + `strictify`, `DEFAULT_OPENAI_MODEL`,
  `LLM_MAX_CALLS`
- `src/handlers/git.rs` — `GitReq`/`GitHandler`, using the bridged records
  (`GitCommit`/`GitStatusEntry`/`GitFileDelta`) and `bridged_records_module`,
  both defined in `tidepool-bridge-effects` and re-exported here
- `src/handlers/time.rs` — `TimeReq`/`TimeHandler`
- `src/handlers/meta.rs` — `MetaReq`/`MetaHandler` (debug path only)
- `src/handlers/event.rs` — repository-event subscribe/drain/unsubscribe over
  `exomonad_worktree::EventJournal`; not in the `base_effects!`
  default row
- `src/handlers/worktree.rs` — `WorktreeReq`/`WorktreeHandler` over
  `exomonad_worktree::create`/`registry`/`git`; also not in the
  `base_effects!` default row — see `exomonad/worktree/CLAUDE.md`
- `src/handlers/journal.rs`, `journal_version.rs` — the durable append-only
  run journal, one JSON line per `record`, and its wire-format version stamp on
  `tidepool_repr::version_ladder`
- `src/handlers/source.rs` — reloading a run's own workspace Haskell source.
  The capture, typecheck and publication live in `tidepool::exomonad::source`;
  this module is the effect's request decoding and wire conversion
- `src/handlers/jev.rs` — the Jev HTTP client behind the `Jev` effect
- `src/handlers/agent.rs` — the agent effect's request handling
- `src/handlers/entropy.rs` — process-local randomness for authored programs

`src/lib.rs` keeps the stack assembly (`HandlerConfig`, `handler_for!`,
`build_base_stack`, `build_minimal_stack`, `base_decls`) and
re-exports everything via `pub use handlers::*;`, so the external surface is
flat (`tidepool_handlers::FsReadHandler`). Each module carries its own `#[cfg(test)] mod tests`;
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
(`bridge/mcp/src/effect_defs.rs`) + one inherent method here. A wholly new
effect type needs a new definition, a new module under `src/handlers/`, a
`pub mod` + `pub use` line in `src/handlers/mod.rs`, a `handler_for!` arm in
`src/lib.rs`. Rust dispatch is by the request constructor's nominal identity;
the Haskell union position is not a handler slot.

## `cx.respond` — one structural response boundary

- **`respond(val)`** owns the response until the runtime visits its structure
  directly into managed construction. Lists, records, enums, JSON, and ordinary
  containers all use this path; do not add a second list or eager-value channel.
  For a TYPED per-verb failure (#335), the errors-tagged method returns
  `Result<T, <ErrEnum>>` and takes no `cx`; the generated dispatch arm wraps it
  with `cx.respond` (`Ok → Right v`, `Err → Left e`), so the handler is total by
  construction — no eval abort for a verb-level failure. (A genuine panic or a
  non-`Handler` `EffectError` — real corruption — is the only abort path.)
  The visitor and managed builder keep long list construction iterative. If an
  unbounded source needs exposure, add explicit pagination at the verb level.


## Sandboxing (FsRead/FsWrite) — and Exec's honest non-sandbox

**FsRead and FsWrite share one backend rooted at `HandlerConfig.cwd`** (the workspace/session
sandbox). Path resolution canonicalizes both the sandbox root and the target
path, then checks `starts_with` — any path resolving outside the root is a
loud `"path escape: ... is outside sandbox"` / `"Path escapes sandbox: ..."`
error, not a silent clamp. This is enforced per-call at the handler, not once
at startup — a symlink or `..` component escaping the sandbox is caught
after canonicalization, not before.

**Exec is NOT filesystem-sandboxed.** `ExecHandler` sets the *initial* working
directory of the spawned `sh -c`/argv process to `HandlerConfig.cwd` (or, for
`runIn`, a directory canonicalized to resolve inside it) — that is the entire
containment. The command itself then runs as an ordinary unrestricted host
process: it can `cd` anywhere, read/write any path the OS user can reach,
spawn further children, and make network connections. `resolve_dir`'s
sandbox check only constrains which directory `runIn`'s `dir` argument may
name; it says nothing about what the process does once running. There is no
landlock/seccomp/namespace confinement here, by design (see root `CLAUDE.md`:
Tidepool is a trusted-operator dev harness — **the effect stack is the
capability boundary, not the filesystem**; an agent wired without the `Exec`
effect cannot run shell commands at all, but one wired with it can run
anything the host user can). Exec's actual robustness controls are process-
level, not filesystem-level: bounded streaming output capture, a timeout, and
process-group termination on timeout — see `src/handlers/exec.rs`.

`FsWriteReq::FsWrite` has **mkdir-p semantics**: missing parent directories are
created automatically (`std::fs::create_dir_all`) before writing. Parent
creation is subject to the same sandbox check — the check validates the target
path first, and any ancestor inside the sandbox root is safe by construction.
