`planCleanup` derives a read-only, deepest-first cleanup plan from a retained
fork-group handle. Pending responses or watches are explicit blockers.

```haskell
cleanupPlan <- planCleanup (forkGroupHandle oneWorker)
```

Inspect the plan before executing it. `executeCleanup` forgets only scoped
terminal response/watch metadata, retires descendants inside-out, forgets
their terminal observations, and retires the group record.

```haskell
cleanupReceipt <- executeCleanup cleanupPlan
```

The receipt records every attempted step and refusal. Repeating it is safe.
Cleanup never deletes worktrees, branches, commits, build evidence, or user
files.
