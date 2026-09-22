---
name: exomonad-cleanup
description: Retire Exomonad workers and fork groups deliberately — inspect a cleanup plan, execute it, and read the two-phase stop outcomes. Load before retiring anything, or when a stop reports that resources are still retained.
---

Cleanup is a decision, not housekeeping. Keep valuable specialists; retire a
group when its obligations are settled and you know they are. Inspection and
execution are separate calls so the plan can be read before anything stops.

Given a retained `worker :: Response result`, `planCleanupFor` extracts its fork
group and returns a refusing plan for a response that was never admitted through
`unfold`:

```haskell
cleanupPlan <- planCleanupFor worker
cleanupPlan
```

If you already hold a `ForkGroupHandle` — from `forkGroupHandle` or
`observeForkGroup` — call `planCleanup` on it directly. Inspect the plan, then
execute it in a separate cell:

```haskell
cleanupReceipt <- executeCleanup cleanupPlan
cleanupReceiptPlan cleanupReceipt
```

Execution honors the actor incarnations and activity revisions that were
inspected. A new descendant, request, or watch makes the plan stale, and
`CleanupStalePlan` is a typed refusal: no teardown has begun, and
`cleanupReceiptPlan` shows the current scope to inspect before planning again.
Polling a handle or finishing already-observed work does not make a plan stale.
Pending obligations stay blockers, including a descendant's outbound requests.
Retrying an inspected plan cannot widen its scope; a survivor that has accepted
new work needs a fresh plan.

The typed roster separates request work from provider health. `IdleRetained`
requires a successful, non-stale provider completion and no request work;
`SettledAwaitingProvider` means the request settled while the provider is still
active; `NeedsAttention` covers failed, interrupted, unknown or stale
observations. Cleanup rechecks this immediately before retiring each live
actor. For a stuck or failed worker use `stopAgent` explicitly — a control
action, not a claim that the provider was idle.

## Stopping has two phases

First the actor publishes its terminal state; then the host releases its
process, pane, tool service, socket and workspace view. `stopAgent` and each
`CleanupStoppedActor` step wait for the second phase, and the outcome says
which of them happened:

```haskell
stopped <- stopAgent (responseActor worker)
stopped
```

- `StoppedNow` — both phases completed. This is final; no notice follows.
- `StoppedRetaining detail` — the actor is stopped, and the named resources
  stay retained on purpose. Also final; no notice follows. Read `detail` and
  decide whether anything still needs a repository or host action.
- `StoppedReleasing` — release had not settled within the wait. Exactly one
  later notice reports how it ended. Do not poll for it, and do not re-issue
  the stop: the release is underway and a second call cannot speed it up.

Treat `StoppedReleasing` as a pending fact in your own state, not as a failure,
and continue useful work until its notice arrives.

## Nothing is deleted

Cleanup never deletes worktrees, branches, commits, build evidence or user
files. A dirty worktree stays available after its actor is retired, which is
exactly what you want when the reason for retiring was that the worker was
stuck. Any repository cleanup is a separate, explicit Git decision made after
you have read what is there.

`releaseGroup groupHandle` is the scoped form used during a wave: it asks the
existing cleanup owner to release workers you no longer need and retains
uncertain members instead of cancelling their pending requests. Retain the
receipt; a blocked step leaves that work with its current owner, and that is
information, not an error to retry in a loop. `doc cleanup` carries the same
material in fallback form.
