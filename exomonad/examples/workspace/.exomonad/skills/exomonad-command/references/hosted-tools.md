# Defining compiled tools

Use [exomonad-agent-spec](../../exomonad-agent-spec/SKILL.md) for the canonical tools
record and reload example. Nest `Shell.ShellTools` to retain `bash`, `write_stdin`,
`read_output`, and `cancel_command`; add project tools only for new semantics.

`Tidepool.Command.Tools.execute` implements structured `bash` with the shared
command owner. With no background or yield option, it starts invocation-owned
work, awaits terminal completion and presents output once. `background: true`
starts actor-owned work with a completion notice. Explicit `yield_time_ms` selects
bounded presentation and detaches a still-live job before returning; it installs
no automatic completion notice. Command options are validated before execution.

`Cmd.observe` and its completion-notifying variants never detach. When authoring
a bounded tool, explicitly transfer unfinished work before returning it.
`Cmd.run` preserves the continuation until terminal completion; choose that
ordinary suspended computation when later actions depend on its result.

Schemas and dispatch derive from the same Haskell record. The declared surface
is fixed for an actor incarnation. The run owner can use `reloadSource` and
`reload_agent_spec` to replace its implementation; child checkout edits do not
alter installed run tooling. A reload refuses changed names, descriptions,
argument types, or order. Such changes require a new incarnation.
