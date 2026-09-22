`planCleanup` inspects a retained fork group and its exact descendants. It
requires inspection authority; `executeCleanup` requires control authority.
Given an existing `oneWorker` (a `Response`), use `planCleanupFor`, which
extracts the fork group itself and returns a refusing plan for a response
that was never admitted through `unfold`:

```haskell
cleanupPlan <- planCleanupFor oneWorker
cleanupPlan
```

If you already hold a `ForkGroupHandle` (from `forkGroupHandle` or
`observeForkGroup`), call `planCleanup` directly instead.

Inspect this result, then execute in a separate hosted call:

```haskell
cleanupReceipt <- executeCleanup cleanupPlan
cleanupReceiptPlan cleanupReceipt
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

The typed roster separates current and queued requests from provider health.
`IdleRetained` requires a successful, non-stale provider completion and no
request work. `SettledAwaitingProvider` means the actor's request is settled
but the provider is still active; `NeedsAttention` includes failed,
interrupted, unknown, or stale provider observations. Cleanup rechecks this
evidence immediately before retiring each live actor. For deliberate recovery
of a failed or stuck worker, use explicit `stopAgent`; it is a control action,
not a claim that the provider was idle.

Stopping has two phases: the actor publishes its terminal state, then the
host releases its process, pane, tool service, socket and workspace view.
`stopAgent` and each `CleanupStoppedActor` step wait for the second phase.
`StoppedNow` means both happened. `StoppedRetaining detail` means the actor is
stopped but the named resources stay retained. `StoppedReleasing` means
release had not settled within the wait; one later notice reports how it
ended. No later notice follows `StoppedNow` or `StoppedRetaining`.

Cleanup never deletes worktrees, branches, commits, build evidence, or user
files. Dirty worktrees remain available after actor retirement. Use ordinary
Git and explicit repository operations for any later repository cleanup.

skill: exomonad-cleanup
