---
name: exomonad-agent-spec
description: Use when declaring or reloading the typed tools available to an Exomonad actor.
---

An agent spec selects one Haskell record of tools. Its fields and their types
define the hosted tool names, schemas, and typed inputs and outputs. Nested tool
records compose at the field where they are included. The spec is source in the
actor's checkout; saving a file does not install it.

## Find and check the spec

The conventional module is `AgentSpec`, exporting `agentSpec`. Workspace
configuration may instead set `[haskell] spec`; `[haskell] tools` names a tools
record when there is no agent spec. Use `status` with `view: "detailed"` to see
the selected rule, source file, and installed revision. Run
`exomonad check --workspace <path>` to typecheck a workspace before launch.

## Put behavior in the tool

A tool body receives its declared Haskell input and can use typed effects. Put
judgment or result handling in that body, not in a separate observer of a
completed tool call. When the boundary needs a different presentation, use the
tool's typed seam:

- [`Project.Shell`](../../Project/Shell.hs) selects and presents typed command
  observations and retained output.
- [`Project.Lookup`](../../Project/Lookup.hs) uses typed lookup results and
  candidates for Jev-based selection.

These modules are workspace-specific examples, not functions exported by the
shared agent-spec API. Read their source before adapting them; a tool's effect
constraints and record type must match the effects its body uses.

## Reload

Edit the spec with ordinary workspace file tools, then call
`reload_agent_spec`. It publishes the checkout's source layer and rebuilds
your own spec. A typecheck failure or changed declared tool name, description,
kind, schema, or order refuses the reload and leaves the installed record
active. A tool call already running keeps its implementation. A changed tool
surface takes effect in a new actor incarnation; this reload never changes a
child actor's spec.
