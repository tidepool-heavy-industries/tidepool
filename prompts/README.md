# Prompt artifacts

This tree contains stable, Tidepool-authored model instructions that are compiled into their owning Rust targets with `include_str!`. It is not a runtime configuration directory: there is no prompt loader, template syntax, environment override, or fallback copy.

Files under `shoal/` retain their existing Codex surfaces and roles:

| Artifact | Owner | Consuming role |
|---|---|---|
| `root.md` | `tidepool::actor_host` | Developer instructions for the root actor |
| `recreated-root.md` | `tidepool::actor_host` | Developer-instruction suffix for a retained conversation on a new actor incarnation |
| `worktree-agent.md` | `tidepool::actor_host` | Developer instructions for a worktree-backed child actor |
| `readonly-agent.md` | `tidepool::actor_host` | Developer instructions for a child actor without a coding worktree |
| `haskell-tool-description.md` | `tidepool-actor::resident_interactive` | Hosted-tool description |
| `haskell-tool-instructions.md` | `tidepool-actor::resident_interactive` | Hosted-tool usage instructions |

`harness/system-framing.md` is the System message for ordinary resident typed-yield harness nodes. `selfharness/memory-curator.md` is seeded as the memory repository's `AGENTS.md`; Codex consumes that file as repository-scoped Developer policy. User tasks, Haskell-authored startup values, and operator input never belong in this tree.

## Inventory boundary

- **Artifact:** the files listed above are stable instruction blocks with fixed roles and no runtime facts. Their owning catalogs enumerate them and compile them into binaries.
- **Typed dynamic rendering:** actor goal and lifecycle notices, workbench receipts, typed-hole cards, effect-row help, selfharness corrective turns, round-budget notices, and compaction requests combine closed runtime state with guidance. They remain in their typed Rust renderers; no renderer recovers state by matching its rendered text.
- **Diagnostic:** compiler/workbench/provider failures, setup-mode remediation, command errors, and operator-facing status text remain with the mechanism that detects the condition.
- **Authored task data:** `initial_user_message`, completion tasks, harness prompts, fork briefs, startup values, and operator messages remain User content supplied by Haskell or the operator.

The continuation sentence in `haskell/lib/Tidepool/Agent/Action.hs` remains in place until the activation-atomicity lane gives continuation rendering a Rust owner. The discovery lane is adding `:show imports`; after that API lands, separately review the root and hosted-tool discovery wording and replace the hosted-tool description's already-undefined `assemble` example. Those are wording changes, not part of this byte-preserving extraction.
