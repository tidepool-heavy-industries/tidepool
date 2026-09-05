Shoal tracks distinct supervisor, context-parent, provider-parent, fork-group,
and Git branch relationships. Use the compact view first:

```text
:lineage
```

Use `:status` for current work and failed actors, `:status!` for full terminal
history, and `:trace`
for provider usage samples, prompt fingerprints, exact identities, and deeper
diagnostics. The canonical workspace path is actor-relative; actor, worktree,
and branch identities establish custody.

Each actor entry includes a typed workbench posture. `WorkbenchRunningUnit`
means hosted Haskell is executing; `WorkbenchAwaitingEffect` names the effect
boundary currently suspended in its Rust interpreter. Neither should be
inferred from elapsed time or notification prose.

`observeForkGroup (forkGroupHandle worker)` inspects exact admitted group
ancestry. It returns `Maybe ForkGroupSnapshot`; `Nothing` means that the group
or retained roster is unavailable to this actor. `groupRoster` contains exact
actor incarnations and their observation watermarks. It is an observation of
one frontier and its descendants, not a whole campaign inferred from names.
Compose observations of several groups when your campaign spans several waves.
Git branch-prefix queries select a namespace, not runtime group membership.
