---
name: exomonad-agent-spec
description: Define an Exomonad actor's typed tool record, effect profiles and result presentation, or reload implementations without changing its registered surface. Use when reusable Haskell behavior should become a hosted tool.
---

An agent spec selects one Haskell record of tools. Its fields and their types
define the hosted tool names, schemas, and typed inputs and outputs. Nested tool
records compose at the field where they are included. A tool can expose a useful
function or a command in an actor's control language: its handler can call an
endpoint, ask Jev, or compose several effects and present the resulting value.
Build and explore those functions and machines in the notebook first; the spec
gives selected behavior a named model-facing entry point. The installed spec is
compiled from the run's current source; saving a file does not install it.
Agent-to-agent `request` and record-actor `Call` endpoints already carry Haskell
values directly. They do not need a hosted tool or its JSON schema; use an
`AgentSpec` when a model-facing tool is the interface you want.

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
Keep domain outcomes as algebraic data types through composition and branch on
constructors; select model-facing text with `presentWith` at the hosted boundary.
Use an ordinary function when only code calls it, a tool when the model needs a
named entry point, and a record actor when shared state or continuation must
survive the call. A semantic judgment inside a tool can select a typed action
with Jev; retain its evidence and decision when later work needs to inspect it.

Ordinary tool fields are asynchronous; wrap a `Call`, `RawCall`, or `Notify`
endpoint in `Sync` to hold the caller's next inference until it settles.
A notebook field uses `HaskellCell effects`, optionally wrapped in `Sync`;
`haskellTool` builds its checked profile. Only a synchronous profile can include
`ContextReadWrite`. The installer checks selected effects against this actor's
grants and installed interpreters.

Every hosted tool handler must finish with `presentWith`, which returns an
abstract `Presented handler` carrying the model-facing text choice. The
underlying handler and its semantic result remain typed; the presenter selects
only the text sent to the hosted model. Write `presentWith id $ tool description
handler` for `Text`, `presentWith presentJson $ tool description handler` for
JSON output, or `presentWith presentDisplay` when the existing `Display`
rendering is intended. Apply this finishing constructor to hosted `tool`,
`rawTool`, `syncTool`, `syncRawTool`, and notification fields. A bare hosted
handler is rejected during spec compilation before it can run. Programmatic
actor handlers are not hosted tools and do not need a renderer.

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

Edit the active source, then call `reload_agent_spec`; saving alone does not
activate it. The source owner stages and checks an immutable candidate, then the
actor prepares its installer and compares the registered surface. Source and
handlers become visible together after those checks. A child can prepare edits
in its checkout but cannot publish them into the active run tooling. Supply
`also_check` to widen the checked module set.

Read the receipt. Preparation failure, surface refusal, cancellation before commit,
or a stale candidate retains the previous source and handlers. A durability failure
after visibility reports the new active source and handlers with uncertainty;
do not replay the installer. Draft files remain on disk. Tool names, descriptions, kinds, schemas, scheduling, implementation
kinds, effect profiles and order must match the registered surface. A changed
surface needs a new actor incarnation. Accepted calls keep their implementations,
and a reload never replaces a child's installed spec. Use `reload_helpers` for
actor-local reusable helpers without replacing tool handlers.
