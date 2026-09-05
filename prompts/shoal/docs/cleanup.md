`planCleanup` inspects a retained fork group and its exact descendants. It
requires inspection authority; `executeCleanup` requires control authority.
Given an existing `oneWorker`:

```haskell
cleanupPlan <- planCleanup (forkGroupHandle oneWorker)
cleanupPlan
```

Inspect this result, then execute in a separate hosted call:

```haskell
cleanupReceipt <- executeCleanup cleanupPlan
```

Execution honors the inspected actor incarnations and activity revisions.
New descendants, requests, or watches make the plan stale. `CleanupStalePlan`
is a typed refusal: no teardown has begun, and `cleanupReceiptPlan` shows the
current scope to inspect before planning again. Merely polling a handle or
finishing already-observed work does not make the plan stale. Pending
obligations remain blockers, including a descendant's outbound requests.

Once admitted, cleanup prevents new requests, watches, and descendant forks
from entering the retiring scope. It forgets eligible terminal metadata,
stops actors deepest-first, and retires their group records. If a step fails,
the receipt records the successful prefix and what remains. Retrying the
inspected plan cannot expand its scope; survivors accepting new work require
a fresh plan. Keep valuable specialists rather than cleaning up automatically.

Cleanup never deletes worktrees, branches, commits, build evidence, or user
files. Dirty worktrees remain available after actor retirement. Use ordinary
Git and explicit repository operations for any later repository cleanup.
