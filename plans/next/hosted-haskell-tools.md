# Haskell-backed shell tools

Native-quality shell interaction is authored using `Tidepool.Agent.Contract`.
The path is Codex dynamic tools → startup-compiled Haskell handlers → Commands
effects → existing Rust command/process owners. No callback into Codex's native
exec handlers or separate process/session registry is required.

## Authored interface

`Tidepool.Command.Tools` defines one ordinary record containing raw `bash`,
structured `exec_command`, `write_stdin` and `read_output`. Projects may select
another record through `[haskell] tools = "Project.Tools.tools"`. Declarations,
argument schemas and dispatch are derived together; code freezes at startup.
Invocations supply data to retained compiled functions, not generated source.

Registration is flat: `haskell`, `bash`, `exec_command`, `write_stdin`, and
`read_output` have no project namespace. Shoal disables native shell registration
uniformly across fresh/resumed/forked actors; `apply_patch` remains. The generic
interactive backend selects native or hosted shell ownership explicitly. Codex
accepts external command names when unoccupied and rejects collisions; it does
not silently replace an existing handler. Old live runs keep their frozen tools;
there are no namespaced compatibility aliases in the new surface.

`bashCommand :: Text -> Command` shares construction with the Bash quoter.
Structured handlers compose start, bounded observation, input and read effects.
`Cmd.observe` returns the current status normally; foreground `Cmd.run` retains
its existing interactive overrun binding behavior. Neither cancels on an
observation deadline. Session IDs designate existing opaque jobs; Rust enforces
actor authority. Haskell scope visibility does not transfer grants.

## Interaction contract

- Literal Bash, 256 MiB and a 30-second initial observation by default.
- Structured execution adds cwd, environment, memory, PTY/piped stdin and
  observation/output options. Empty stdin polls without writing.
- Output budgets are bytes. Responses are bounded to 32 KiB with compact
  oversized previews. Display omissions and retained-output gaps are explicit.
- Direct output navigation never executes or waits. Explicit pages are
  non-consuming; incremental observations acknowledge fully displayed pages.
- Typed outcomes distinguish exit, OOM, cancellation and uncertain cleanup.
  A transport failure never licenses an automatic replacement execution.
- Bash does not load login profiles. Native approval hooks, numeric session
  aliases and selected-shell policies are not copied or advertised.

## Acceptance and delivery

Focused resident tests exercise default and renamed project-defined tools,
continued effects after bounded waits, frozen code, input/schema validation,
nominal constructor identity, same-call replay, output recovery and authority.
Matched actual TUI/Shoal binaries use a scripted local provider for PTY input,
nonzero exit, direct retained reads, OOM survival and steering. Inspect the next
provider request to verify what the model actually received.

Changes land on main; unfinished product candidates remain on their branches.
Prepare a matched immutable runner and curated package after focused acceptance.
Do not hot-change the live swarm. No paid inference or full workspace battery.
