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
- `src/handlers/git.rs` — `GitReq`/`GitHandler`, using the bridged records
  (`GitCommit`/`GitStatusEntry`/`GitFileDelta`) and `bridged_records_module`,
  both defined in `tidepool-bridge-effects` and re-exported here
- `src/handlers/time.rs` — `TimeReq`/`TimeHandler`
- `src/handlers/meta.rs` — `MetaReq`/`MetaHandler` (debug path only)
- `src/handlers/event.rs` — repository-event subscribe/drain/unsubscribe over
  `tidepool_worktree::EventJournal` (PRD 19); not in the `base_effects!`
  default row
- `src/handlers/worktree.rs` — `WorktreeReq`/`WorktreeHandler` over
  `tidepool_worktree::create`/`registry`/`git` (PRD 19); also not in the
  `base_effects!` default row — see `tidepool-worktree/CLAUDE.md`

`src/lib.rs` keeps the stack assembly (`HandlerConfig`, `handler_for!`,
`build_base_stack`, `build_minimal_stack`, `base_decls_with_ask`) and
re-exports everything via `pub use handlers::*;`, so the external surface is
flat (`tidepool_handlers::FsHandler`). Each module carries its own `#[cfg(test)] mod tests`;
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
  construction — no eval abort for a verb-level failure. (A genuine panic or a
  non-`Handler` `EffectError` — real corruption — is the only abort path.)
- **`respond_list(vec)`** — an owned `Vec<T>` returned as a Haskell list.
  Every element converts eagerly at dispatch time; what stays special is
  that the machine builds the heap spine ITERATIVELY (stack safety on long
  lists — `host_fns::list_materialize`). The Fs `readGlob` verb
  (`fs_read_glob`, `src/handlers/fs.rs`) is the live call site. There is no
  lazy/streaming response channel; if an unbounded source ever needs
  exposure, add explicit pagination at the verb level.

## Subagent handler: six verbs, one saga, a bounded cycle table

`SubagentHandler` (`src/handlers/agent.rs`) serves six verbs over ONE saga
(`tidepool_agent::spawn`), in three shapes:

- `SubagentSpawn` — the whole saga behind one blocking call.
- `SubagentBegin`/`SubagentResume` — the same saga driven one stop at a time,
  for an agent that holds dynamic tools.
- `SubagentSpawnAsync`/`SubagentAwait`/`SubagentCancel` — the same saga
  detached onto its own thread.

None of them is a second implementation: all three are combinators over
`CycleSaga`, and Haskell's `spawnAgent` is itself `spawnAsync` + `awaitAgent`.

**The cycle table.** The handler holds one entry per admitted cycle, keyed by a
`CycleId` it mints monotonically (what an authored `AgentHandle` wraps).
An entry is either `Stepped` (a saga + its backend, driven inline by
`SubagentResume`) or `Async` (its own thread, a result receiver, and the
canceller taken from its backend *before* the thread started). Backends are
PER-CYCLE, from a `Box<dyn AgentBackendFactory>`: an `AgentBackend` is a step
function over one live thread, so two cycles sharing one would interleave their
replies. `SubagentHandler::new` still takes a single pre-built backend and
wraps it in a ONE-SHOT factory — its second cycle fails `BackendUnavailable`
naming the wiring, which is a fact about that wiring and not a policy refusal.
`with_backends(..)` is the N-cycle constructor.

**The capacity bound.** `with_cycle_capacity(n)` (default 8) counts
NON-TERMINAL entries. A spawn past it is `SpawnCapacityExhausted { capacityLimit }`,
refused immediately with nothing allocated behind it — a BOUND, not a backlog,
so an operator sees the ceiling instead of an invisible queue. Terminal entries
are RETAINED (an await on a finished cycle must stay distinguishable from an
await on a typo'd id) but occupy no slot.

**Cancel settles; it does not merely kill.** `SubagentCancel` reaps the
backend, joins the thread, and THEN takes the substrate mutex briefly to settle
the binding `Released` — that order, because settling first would hold the lock
across a reap of unknown duration. Retain-first is locked: nothing is deleted.
The verb is total (an unknown, already-awaited, or already-cancelled handle is
a no-op), and a cancelled cycle stays in the table as a terminal entry carrying
`SpawnCancelled`. Cancel and await race in either order without hanging or
panicking; the matrix is pinned by `handler_cancel_await_races_reach_typed_terminals`.

The shape worth knowing before you touch the stepped path: a child's tool call
comes back to the parent as a RESULT (`StepToolCall`), the parent's authored
Haskell handler runs between two effect calls, and `agentResumeRaw` answers it.
The Rust handler never runs a parent handler and never re-enters the JIT — it
cannot (`EffectHandler::handle` has no machine handle). See
`tidepool-agent/CLAUDE.md` for the seam and
`plans/post-restart/agent-lanes/lane-codex-live-plan.md` §2 for why.

Two consequences that survive the cycle table unchanged:

- **Model tier and effort are HANDLER CONFIGURATION** (`with_model_policy`),
  not an authored-surface field. A model budget is granted to an operator, and
  the operator is who wires the handler; an authored call choosing its own tier
  would let any eval spend at any price. (A semantic tier vocabulary on the
  authored surface is PRD 18 open decision 3, still open.)
- **The handler owning the backends is what bounds a parked child.** Cycle-scoped
  and not `Clone` (the `RepoEventHandler` precedent): it owns the cycle table
  and a flocked binding table. Dropping it kills the backend processes — which
  is the ONLY thing bounding a child parked on an unanswered tool call — so its
  `Drop` reaps every live async cycle rather than orphaning threads, and a
  durable mailbox or a cross-cycle agent still cannot live here.

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
