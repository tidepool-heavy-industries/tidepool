---
name: exomonad-agent-spec
description: Use when declaring or reloading the typed tools available to an Exomonad actor.
---

An agent spec selects one Haskell record of tools. Its fields and their types
define the hosted tool names, schemas, and typed inputs and outputs. Nested tool
records compose at the field where they are included. The installed spec is
compiled from the run's current source; saving a file does not install it.

## Find and check the spec

The conventional module is `AgentSpec`, exporting `agentSpec`. Workspace
configuration may set `[haskell] spec` to another exported spec. The conventional
module takes precedence; with neither, the host installs the default notebook
spec. A host with context-editing support exposes `haskell` and `haskell_sync`;
other hosts expose only `haskell`. The obsolete `[haskell] tools` key is rejected;
put the typed tools record in an `AgentSpec` and configure its exported value.
The spec comes from the run's current tooling graph, not a child's historical
checkout. Use `status` with `view: "detailed"` to see the selected
rule, source file, and installed revision. Run
`exomonad check --workspace <path>` to typecheck a workspace before launch.

## Put behavior at the right boundary

A tool body receives its declared Haskell input and can use typed effects.
Ordinary tool fields are asynchronous; wrap a `Call`, `RawCall`, or `Notify`
endpoint in `Sync` to hold the caller's next inference until it settles.
A notebook field uses `HaskellCell effects`, optionally wrapped in `Sync`;
`haskellTool` builds its checked profile. Only a synchronous profile can include
`ContextReadWrite`. The installer checks selected effects against this actor's
grants and installed interpreters.

Every named handler in an installed spec must choose how its semantic result
becomes model-facing text. Write `presentWith id $ tool description handler`
for `Text`, `presentWith presentJson $ tool description handler` for JSON
output, or `presentWith presentDisplay` when the existing `Display` rendering
is intended. Apply the same wrapper to `rawTool`, `syncTool`, `syncRawTool`, and
notifications. If a named handler has no presenter, spec compilation refuses
it before running a handler.

When the tool needs to select or present its own result, use that tool's typed
seam:

- [`Project.Shell`](../../Project/Shell.hs) selects and presents typed command
  observations and retained output.
- [`Project.Lookup`](../../Project/Lookup.hs) uses typed lookup results and
  candidates for Jev-based selection.

These modules are workspace-specific examples, not functions exported by the
shared agent-spec API. Read their source before adapting them; a tool's effect
constraints and record type must match the effects its body uses.

An after-tool hook can observe a completed call; it does not present or rewrite
that tool's result. `toolResultValue` is the semantic JSON result, while
`toolResultOutput` is the selected text. The shipped spec does not install a
blanket monitor.
For a specific workflow, prefer an explicit bounded policy over source-identified
episodes using [`Project.WorkflowReminders`](../../Project/WorkflowReminders.hs).
Keep experimental judgments in shadow mode until evaluated; no judgment is proof
of acceptance or completion. Use a tool's typed seam for result presentation.

## Reload

The run owner edits the run workspace, calls `reloadSource`, then calls
`reload_agent_spec` to rebuild its own spec. A child can prepare edits in its
checkout, but cannot publish them into the active run tooling. A typecheck
failure or changed declared tool name, description, kind, schema, scheduling,
implementation kind, effect profile, or order refuses the reload and leaves the
installed record active. A tool call already running keeps its implementation.
A changed tool surface takes effect in a new actor incarnation; this reload
never changes a child actor's spec.
