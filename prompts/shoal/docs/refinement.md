Actors are retained handles, not one-shot tasks. Use a retained actor when its
learned context and worktree are useful for a focused revision. Its context does
not automatically acquire the parent's later reasoning: put accepted findings,
changed constraints, and the evidence you need into the new typed input. Fork
again when the updated parent context is the better starting point.

With `interfaceWorker`, `revisionLabel`, and a task-specific `revisionPlan`
already bound, and `RevisionReport` defined as your desired result type:

```haskell
:{
revision <- requestWith @RevisionReport (forkedActor interfaceWorker) $
  withRequestGuidance "Address only the accepted review findings." $
  requestOptions revisionLabel revisionPlan
:}
```

The new response has its own identity and worktree evidence. Settling either
request does not terminate the actor; teardown remains an explicit supervisor
decision.

A reviewer can drive this same follow-up directly. The parent first receives
the implementer's candidate and lets that request settle, then forks a reviewer
from its current context. Give the reviewer the candidate, contract, and
`interfaceWorker` (or just its `AgentRef`). The reviewer authors `revisionPlan`
and owns the new `revision` response. It does not poll the parent's response or
use the parent's reply authority.

After submitting the repair above, the reviewer registers its own watch:

```haskell
let Right repairReadyLabel = watchLabel "repair-ready"
repairReady <- watch repairReadyLabel (awaitResponse revision)
```

End the model turn without settling the review request. On wake, poll
`repairReady`, inspect the returned candidate and execution/worktree evidence,
and review the revised commit. Repeat for within-contract repairs; settle the
original review request with acceptance or a precise decision for the parent.
Use a settled dependency instead when you want to fold failure with other
independent evidence; unavailable watches also remain inspectable typed state.

The implementer returns a revised candidate or a typed clarification/decision
need in its repair reply. The reviewer can answer in the next request. Do not
make a circular wait: the reviewer already has an active request while it waits
for repair, so a new request back to it would queue behind that work. Progress
publication does not settle that request or change its reply ownership.

Keep code ownership with the implementer and review in the reviewer's own
permitted checkout. A reviewer who runs checks needs coding authority, not an
inspection-only role. Sharing an actor reference does not grant access to its
worktree or transfer response, watch, or settlement ownership. Cancelling the
review alone does not establish that a peer repair stopped; observe and settle
or cancel that work through its owner before declaring the loop quiescent.

A baseline-incorporation follow-up should carry the accepted commit and the
consequential delta, for example: “Input adapter now owns paste delivery; merge
this baseline and update routing.” Keep the rationale in the committed design.
Ask for the resulting head, conflicts or unresolved choices, and checks performed
on that head. Receipt of the assignment is not evidence of incorporation.
Prefer merging an accepted baseline into already published work so earlier
candidate identities remain traceable; rebasing unpublished work can be appropriate.
The next review names the new candidate explicitly.
