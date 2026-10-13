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
The direct background path also captures starting source for the notice, using
a separate admitted probe. `Cmd.background` does not request that probe unless
its command is wrapped in `Cmd.withSource`.

`Cmd.observe` and its completion-notifying variants never detach. When authoring
a bounded tool, explicitly transfer unfinished work before returning it.
`Cmd.run` preserves the continuation until terminal completion; choose that
ordinary suspended computation when later actions depend on its result.

Schemas and dispatch derive from the same Haskell record. The declared surface
is fixed for an actor incarnation. Edit source and call `reload` (or Haskell
`reloadSource`) to publish compatible implementations together with workspace
prompts. A changed generation resets notebook bindings; unchanged reload preserves
them. Changed tool names, descriptions, argument types or order refuse publication
and require a new incarnation. Existing work and children retain their issued
generation; editing a child checkout does not update previously issued work.
