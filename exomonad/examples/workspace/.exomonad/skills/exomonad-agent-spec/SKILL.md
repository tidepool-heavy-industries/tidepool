---
name: exomonad-agent-spec
description: Define an Exomonad actor's typed tools, effect profiles and result presentation, or publish source and tool changes through an authored synchronous reload field. Use when reusable Haskell behavior should become a hosted tool.
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

Every hosted function handler field (`Call`, `RawCall`, `Notify`, and their
`Sync` forms) in an installed spec must use `presentWith`, which returns an
abstract `Presented handler` carrying the model-facing text choice. The
underlying handler and its semantic result remain typed; the presenter selects
only the text sent to the hosted model. Write `presentWith id $ tool description
handler` for `Text`, `presentWith presentJson $ tool description handler` for
JSON output, or `presentWith presentDisplay` when the existing `Display`
rendering is intended. A bare handler fails Haskell typechecking because the
hosted field requires `Presented`. Native `HaskellCell`/`HaskellTool` fields
and programmatic actor handlers do not use this wrapper.

When the tool needs to select or present its own result, use that tool's typed
seam:

- [`Project.Shell`](../../Project/Shell.hs) selects and presents typed command
  observations and retained output.
- [`Project.Lookup`](../../Project/Lookup.hs) uses typed lookup results and
  candidates for Jev-based selection.

These modules are workspace-specific examples, not functions exported by the
shared agent-spec API. Read their source before adapting them; a tool's effect
constraints and record type must match the effects its body uses.

An after-tool hook observes a completed executed call. Compile, input-schema and
other preexecution refusals do not invoke it. It does not present or rewrite
that tool's result. `toolResultValue` is the semantic JSON result, while
`toolResultOutput` is the selected text. The shipped spec does not install a
blanket monitor.
For a specific workflow, prefer an explicit bounded policy over source-identified
episodes using [`Project.WorkflowReminders`](../../Project/WorkflowReminders.hs).
Start with a small observable change and retain the original tool result and
judgment. Use representative actual outcomes to refine the policy; a judgment
does not establish acceptance or completion. Use a tool's typed seam for result
presentation.

## Reload

Reload is an ordinary synchronous `Call` field in the authored spec. Shipped
editable specs name it `reload` and use `standardReload`; another field name works
with the same handler. Its field type is
`mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))`.
Edit ordinary `.hs` definitions and workspace prompts, call that field, and read
its typed receipt before relying on the new generation. Saving alone does not
activate a draft. Source, tools and workspace prompts publish together.

`also_check` adds modules to validation. `accept_cache_invalidation` defaults to
false: an incompatible tool surface is refused with its diff. Explicitly setting
it to true permits supported tool additions, removals, renames, reordering and
schema changes, after validation of the next provider request. Authority checks,
typed actor protocols and provider schema restrictions still apply.

Only an admitted synchronous hosted invocation can use `Reload`. Synchronous
notebook cells and ordinary `Sync` function fields share this owner; asynchronous
endpoints and programmatic calls cannot activate drafts.
`reloadHandler` and `reloadPresenter` compose in a custom field; `reloadSource []`
is the compatible-only Haskell operation in a synchronous notebook or handler, and
`reloadSourceWith` accepts typed options. Synchrony alone does not grant source
ownership. Captured-only actors remain pinned.

Changed configuration creates a fresh notebook; unchanged configuration preserves
bindings. Keep durable definitions in `.hs` files. Reload preserves conversation,
resources and published memory context, and does not rerun startup or refresh memory.
Preparation failure, refused surface changes or typed actor protocols, cancellation
before commit, and stale candidates retain the previous generation. A confirmed
commit with a projection repair warning is committed; uncertain state requires
reconciliation rather than replay. Draft files remain on disk.

Accepted calls and existing children retain their issued generation. Fresh-context
children select the published source. Source-backed actors edit within declared
roots, without older workspace-source fallback. Package pins, source roots,
external services and capability grants remain launch inputs.
