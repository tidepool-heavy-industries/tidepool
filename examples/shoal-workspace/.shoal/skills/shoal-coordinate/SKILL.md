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

In the parent, given `worker :: Forked (Outcome Candidate)` and its `progress`
handle, keep one local collector. The handler receives changes without rearming:

```haskell
import qualified Tidepool.Actor as Actor
owner <- actorContext
router <- followWork [("implementation", forkedResponse worker, progress)] (notifyWork owner (workMessage candidateSummary))
state <- Actor.call router WorkSnapshot
inspectFull (workSnapshotSummary candidateSummary state)
```

`workSnapshotSummary :: (value -> Text) -> WorkState value -> Text` shows source
status, candidate commits, question keys/sources and final results. It does not
consume state. Inspect `collectedWork state` for full evidence or `workNotices state`
for delivery receipts when needed. Call it for a decision, not as a polling ritual.
`withCheckpoints (workMessage candidateSummary)` additionally notifies on candidate
publications when the parent needs to act on them before the final response.

`sendMessage` is ordinary steering; `updateRequest (forkedResponse worker)` is
for a correction that must fence that pending request. Inspect delivery when the
next action depends on it. Neither admission nor presentation proves the requested
change happened. Reply only with information needed for the next decision.

When this collector's work is finished, `Actor.drainActor router` followed by
`Actor.awaitExit router` closes it. Don't retire it merely because a model turn ends.
