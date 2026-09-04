Shoal tracks distinct supervisor, context-parent, provider-parent, fork-group,
and Git branch relationships. Use the compact view first:

```text
:lineage
```

Use `:status` for current work, `:status!` for terminal history, and `:trace`
for provider usage samples, prompt fingerprints, exact identities, and deeper
diagnostics. The canonical workspace path is actor-relative; actor, worktree,
and branch identities establish custody.

Each actor entry includes a typed workbench posture. `WorkbenchRunningUnit`
means hosted Haskell is executing; `WorkbenchAwaitingEffect` names the effect
boundary currently suspended in its Rust interpreter. Neither should be
inferred from elapsed time or notification prose.
