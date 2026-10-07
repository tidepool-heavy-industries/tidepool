A hosted child can share writable files with its parent or start in a registered
workspace attachment. `SameDir` shares the actual files, index, and HEAD. It does
not select compiled source or install tools. `ExistingWorkspace` reuses a backing
workspace through an opaque run-issued handle. `ForkWorktree seed` resolves one
selected committed seed and creates a separate worktree attachment.

Context and workspace are independent choices. `ForkCtx checkpoint` uses an
explicit captured conversation snapshot. `FreshCtx prompt` starts from an
explicit prompt without ambient lexical bindings; closures may still carry
explicit dependencies. An optional text label describes the actor but does not
name its identity or workspace.

A child's actor attachment has its own lifetime. Retiring it does not retire
sibling attachments or delete the directory. Workspace identity, actor identity,
request ownership, and cleanup are separate facts. Inspect the current typed
workspace and actor observations before using source evidence. A child result
that names a commit is inspectable from the parent's Git view after that object
is available; never read files through a live child's worktree attachment.

For Git delivery and exact-candidate review, load `exomonad-project-work`.
