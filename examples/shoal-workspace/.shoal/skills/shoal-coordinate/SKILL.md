---
name: shoal-coordinate
description: Route and inspect Shoal child progress and final replies with Project.Routing actors, without manual watch rearming or repeated full-state reports.
---

For a worker launched with `childWithProgress @WorkProgress`, its current request
installs `reportProgress`. Given `candidate :: Candidate`, publish useful evidence
without ending the request:

```haskell
reportProgress (WorkProgress [candidate] [])
```

The list of questions is the current unresolved set, not only newly opened ones.
Successful progress is evidence, not a Question. A plain `child` request has no
progress stream; use its requested final `respond` or ordinary steering when useful.
A parent's `reportProgress` in inherited history does not belong to the child.

In the parent, given `worker :: Response (Outcome Candidate)` and its `progress`
handle, keep one local collector. The handler receives changes without rearming:

```haskell
import qualified Tidepool.Actor as Actor
router <- followWork [("implementation", worker, progress)] (notifyWork me (workMessage candidateSummary))
state <- readWork router
inspectFull (workSnapshotSummary candidateSummary state)
```

`workSnapshotSummary :: (value -> Text) -> WorkState value -> Text` shows source
status, candidate commits, question keys/sources and final results. It does not
consume state. Inspect `collectedWork state` for full evidence or `workNotices state`
for delivery receipts when needed. Call it for a decision, not as a polling ritual.
`withCheckpoints (workMessage candidateSummary)` additionally notifies on candidate
publications when the parent needs to act on them before the final response.

`sendMessage` is ordinary steering; `updateRequest worker` is
for a correction that must fence that pending request. Inspect delivery when the
next action depends on it. Neither admission nor presentation proves the requested
change happened. Reply only with information needed for the next decision.

Record handled candidates explicitly with
`R.send (incorporatedWork (R.client router)) ("implementation", [candidate])`.
The normal brief omits those exact entries; changed evidence at the same commit
remains outstanding. Candidate event indices select the original publications in
`workHistory state` (for example `take 1 (drop index (workHistory state))`).

When the integration cycle is finished, `finishWork router` drains the collector
and returns its retained final state. Keep it through repairs; a model turn ending
is not a reason to retire it. `releaseGroup groupHandle` separately asks the existing
cleanup owner to release workers you no longer need, retaining uncertain members:

```haskell
retiredWork <- finishWork router
let Just group = forkGroupHandle worker
released <- releaseGroup group
inspectFull released
```

Retain the cleanup receipt; a blocked step leaves that work with its current owner.
Do not turn it into a stop/retry loop.

For custom typed joins or automatic request continuations, load
`shoal-define-actors`. Routine routing stays in Haskell; wake the owning Sol only
for engineering decisions, actionable failures or integration work.
