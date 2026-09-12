# A known review handoff without a relay turn

Use this when the owning plan calls for independent review and a retained reviewer
is available. The owning Sol chooses work, repairs defects and checks integration.
Load `shoal-define-actors` for the record syntax.

The [executable composition](../checks/review-continuation.hs) defines one
`ReviewFlow` record with private state, typed message endpoints and a fixed
candidate-result source. It uses `coordinationActor`, the project's selected
`Replies`, `Actor`, `Notifications` row. It is not a mandatory worker stage.

Supply these bindings:

- `task :: Task`, the incorporated assignment;
- `worker :: Response (Outcome Candidate)`, the pending implementer;
- `reviewer :: Response (Outcome ReviewDecision)`, an available retained reviewer;
- `reviewLabel, repairLabel :: Label`, labels for the exact requests;
- `repairPolicy :: RepairOwner`, `OwnerRepairs` or an explicitly selected available
  `RetainedImplementer`;
- `onReview :: WorkSink (Outcome ReviewDecision)`, the local notification policy;
- `onStopped :: Settlement (Outcome Candidate) -> Maybe Text`;
- `owner :: ActorContextInfo`, captured in the Sol owner's turn.

For ordinary steering back to that owner:

```haskell
import qualified Data.Text as T
owner <- actorContext
let onReview = notifyWork owner (workMessage reviewSummary)
let onStopped outcome = Just ("implementation: " <> either (T.pack . show) candidateSummary (settledValue outcome))
```

A produced candidate submits a new request to the existing reviewer. A blocked or
unavailable implementation retains its original receipt and does not request review.
`requestWithProgressInto` enqueues the exact response/progress handles at
`reviewStarted` before admitting the request. That handler creates the collector
with the receiving incarnation's own return endpoint. Those handles therefore do not rely
on the submitting handler successfully committing its final state. The callback
must retain the handles, not merely print or inspect them. Admission failure does
not authorize repeating the request.

The collector forwards changes and the exact terminal result to `reviewEvent`.
Once that result is handled, the flow drains the attempt's collector and retains
its final state. No model turn rearms a watch, forwards a verdict or maintains a
list of collectors to retire. The integration actor remains available for queries
and the owner's next useful continuation. With `RetainedImplementer`, a repair
verdict directly submits the repair to that worker; its finite `forwardResult`
returns the repaired candidate to the same flow for another review. No new agent
is launched for these repeated requests. Suppress routine repair notifications in
`onReview` when this edge is already declared; retain questions, final acceptance
and failures that need the owner. `OwnerRepairs` instead leaves the engineering
choice with the owning Sol.

```haskell
flow <- R.call (reviewView (R.client reviewBox)) ()
inspectFull (reviewEvents flow, stoppedCandidates flow)
```

A parent snapshot does not flush descendants. React to actual routed results,
not elapsed time or a parent's empty queue. Full runtime result evidence stays
attached; no separate settlement watch is needed.

After all relevant review results have arrived and the owner has incorporated the
verdicts or retained actionable blockers:

```haskell
retiredFlow <- R.finish reviewBox
```

Keep the returned exit and the reviewer agent independently. `R.finish` does not
cancel pending review requests or retire specialists. If work is still pending,
keep the flow alive. A failed handler retains its last committed state; replace
it with the same record schema after inspecting the exact request and queued
continuation. Do not replay uncertain effects. Existing distributed endpoints
remain exact: if replacing a flow that already owns active forwarders, inspect
and rebuild those routes against its retained original responses rather than
resubmitting work or assuming the old endpoints changed identity.
