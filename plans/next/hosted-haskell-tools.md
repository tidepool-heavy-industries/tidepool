# Haskell-backed native tools

Provide native-quality shell interaction through startup-compiled Haskell
handlers. Ordinary command execution must not require knowledge of Haskell or
workbench binding conventions. Commands retain the existing process, resource,
authority, cancellation, output and hosted-call owners.

## Deployment and execution

- One frozen declaration set across roles, beside the reserved `haskell` tool.
- Optional `[haskell] tools = "Project.Tools.tools"` selects a Haskell tools
  record from the captured workspace. The default record provides Bash.
- Extend `Tidepool.Agent.Contract` with `RawCall result` and `rawTool`.
  Structured `Call` inputs remain available. Reuse the Generic declaration
  traversal; do not add a second tool DSL.
- Compile the dispatcher at actor initialization. A private effect adapter
  receives invocation data and lifts the authored handler's normal effect row.
  Each call applies retained code; there is no GHC/source compilation per call.
- Route calls through the same serialized workbench admission and replay receipt
  path. Tool definitions express intent; Rust enforces concrete authority.
- `Cmd.bashCommand :: Text -> Command` shares construction with `[bash|…|]`.
  Scripts are data, including multiline text and Haskell-looking delimiters.

## Shell experience

Native behavior is the comparison baseline: ordinary execution and output,
nonzero exits, live jobs, stdin/PTY interaction, observation limits, and
recoverable output. Keep the agreed 256 MiB default and 30-second foreground
window. Preserve the full native tool surface where options are needed; do not
claim parity merely because a single raw command works.

Successful small calls display directly without automatic result bindings.
Overruns, uncertain output and oversized output retain a real `jobN :: Cmd.Job`
in the calling model's workbench. Bindings must survive display failure and
must not grant another actor authority. Automatic shell responses are bounded
to 32 KiB; oversized results use an 8 KiB beginning/end preview and explicit
retained-output navigation. Never rerun a command to recover output.

## Acceptance

1. Raw and structured declarations, names, reserved names and qualified config.
2. Startup compilation, repeated data-only calls, literal Unicode/multiline
   input, frozen definitions and actor/source isolation.
3. Same-call replay versus changed-payload rejection; failure after effects;
   cancellation, observation timeout, cleanup and live Haskell recovery.
4. Useful small output, nonzero exit, stdout/stderr, bounded large output and
   deliberate navigation without reexecution.
5. Actual matched Codex TUI and Shoal binaries with a scripted local provider;
   inspect the next provider request to verify what the model receives.
6. Update the shared prompt/command skill around the direct tool, with Haskell
   for retained values and composition. Keep the live swarm's package frozen.

## Native parity boundary

The compiled-handler/raw-Bash slice has focused execution evidence: raw and
structured dispatch, source freezing, replay rejection, large Unicode output,
foreground handoff, and the published guide examples. The matched native-TUI
fixture also passed with real binaries and a scripted local provider, covering
OOM, cancellation, steering, raw Bash and retained-output recovery without a
second execution. These checks do not establish the structured native-tool
parity described below.

The raw shortcut and compiled-handler mechanism are one implementation slice.
Full native parity also requires the familiar structured execution/input controls;
Haskell-only remedies do not satisfy that interface requirement.

Compare against the matched Codex checkout's
`core/src/tools/handlers/unified_exec/{exec_command,write_stdin}.rs` and
`core/src/tools/handlers/unified_exec.rs`, under `codex-rs/`:

- Execution options: working directory, selected shell/login behavior, PTY,
  observation window and output allowance, plus applicable permission fields.
- Retained input/polling: exact session identity, stdin, incremental output,
  finished status and interrupted observation without duplicate execution.
- Execution context: shell configuration, applicable hooks and approvals,
  cancellation, and native process/resource ownership.

The existing command backend reaches the owning TUI's
`host_dynamic_tools/commands.rs` and its app-server `OneOffCommandExec` path.
That is not the full `exec_command` tool handler. Preserve the native owner of
those semantics when exposing structured calls; do not copy its argument,
approval or shell policy into the Haskell library. Keep `Cmd.Job` recovery tied
to the same command, without a second process registry.

Work belongs on main. Applications/sleep candidates retain their existing
owners and integrate at a later boundary. No engine refactor, new process
registry, MCP raw-input shim, paid inference fixture or full test battery.

## Remaining implementation sequence

Native reference: Codex `80e36633f515b03e11189e8516be21065e73335e`.
The implementation must cross the native **tool** boundary, not just call a
process launcher with similarly named arguments.

1. **Preserve the invoking native context.** Extend the existing pending dynamic
   call in `core/src/state/turn.rs` to retain the invoking `StepContext`,
   cancellation and diff tracker while the Haskell handler is running. Do not
   reconstruct these from the latest thread settings: `StepContext.tool_router`
   is the actual sampled tool plan, including availability and shell options.
   Avoid an `Arc<Session>` cycle inside the session's own pending-call map.
   Route a narrowly typed execution/input request through that captured router;
   reject absent, completed, foreign-thread or cancelled parent calls before
   invoking a tool. Do not expose unrestricted recursive dynamic-tool dispatch.
2. **Reuse native execution and input handlers.** The production path must reach
   `ExecCommandHandler` and `WriteStdinHandler` through the registry, preserving
   hooks, approval handling and native structured output. Publish their actual
   configured schemas rather than reproducing option policy in Haskell.
   The compiled Haskell handler remains the authored entry point. Native tool
   requests and results cross its effect boundary as data, without compilation.
   Keep the raw Bash shortcut alongside the structured native surface.
3. **Join exact process custody and retained output.** Extend the existing
   command/native process owners so the native session and `Cmd.Job` designate
   one process. A numeric native session ID alone is insufficient: native IDs
   can be reused after removal, and `write_stdin` already revalidates process
   identity after approval. Preserve that invariant through later Haskell
   inspection, input and cancellation. Native output collection drains its
   buffer; retain output at the owning capture boundary, before draining or
   formatting, for bounded non-consuming Haskell reads. Do not reconstruct
   retained output from truncated model-facing responses. Continue enforcing
   Shoal memory admission and resource cleanup at the actual launch boundary.
4. **Wire and verify the complete surface.** Connect the native owner through
   the existing owning-TUI transport and command interpreter. Execution and
   stdin must remain usable directly, without discovering a Haskell binding.
   Automatic bindings supplement that interface for composition and recovery.
   Preserve the agreed 256 MiB/30-second defaults and bounded presentation.

Each stage must acquire a production consumer before being called complete.
The native boundary is part of this task; it is not deferred to the separate
applications megatask. Coordinate file ownership before changing overlapping
native files, and keep the live runner immutable.

### Differential acceptance

Use the same scripted provider and actual native binaries for direct and
Haskell-backed calls. Compare observable behavior, allowing only the agreed
resource/foreground defaults and additional retained-job affordances.

| Boundary | Required evidence |
|---|---|
| Input and discovery | Configured native schemas; workdir, shell/login, PTY, output budget and applicable permission fields accepted/rejected identically |
| Execution context | Captured shell/environment and hooks; actual approval acceptance/rejection; no use of newer thread settings during a suspended handler |
| Background interaction | Direct session receipt; input and empty polling; incremental output; terminal exit; no duplicate execution after interrupted observation |
| Custody | Retained job controls the original process; retired/reused numeric session cannot redirect it; foreign actor cannot acquire control |
| Output | Native response shape and bounds; Unicode; native polling followed by retained Haskell reads; explicit retention gaps |
| Resources and failures | Real command OOM leaves TUI alive; cancellation and cleanup; rejected/stale parent invocation starts no process; transport uncertainty does not trigger retry |
| Composition | Compiled handler can consume a native result and perform another effect; ordinary calls need no Haskell discovery; later Haskell uses the same job |

Run focused native/core and bridge checks first, then the matched TUI fixture.
The existing raw-tool fixture alone cannot prove this matrix.
