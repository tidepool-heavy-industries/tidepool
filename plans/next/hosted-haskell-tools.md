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
