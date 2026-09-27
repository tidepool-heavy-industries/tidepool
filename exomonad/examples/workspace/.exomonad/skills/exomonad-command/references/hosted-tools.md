# Defining compiled tools

Use [exomonad-agent-spec](../../exomonad-agent-spec/SKILL.md) for the canonical tools
record and reload example. Nest `Shell.ShellTools` to retain `bash`, `write_stdin`,
`read_output`, and `cancel_command`; add project tools only for new semantics.

`Shell.execute` implements structured `bash` with `Cmd.tryStart` and bounded
`Cmd.observe`, so an observation expiry returns a retained job and the handler
can continue. `Cmd.run` instead uses foreground handoff when observation expires.
Choose between those execution semantics explicitly when authoring a new tool.

Schemas and dispatch derive from the same Haskell record. The declared surface
is fixed for an actor incarnation. The run owner can use `reloadSource` and
`reload_agent_spec` to replace its implementation; child checkout edits do not
alter installed run tooling. A reload refuses changed names, descriptions,
argument types, or order. Such changes require a new incarnation.
