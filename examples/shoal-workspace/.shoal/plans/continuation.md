# A known review handoff without a relay turn

Use this when the owning plan already calls for independent review and an existing
reviewer is available. Sol still chooses the work, repairs defects and checks
integration. A Produced Candidate may start review; Blocked or unavailable execution
must retain its original receipt for the owner instead.

The [executable composition](../checks/review-continuation.hs) is ordinary resident
Haskell. It defines a small typed mailbox for review events, callback-created
collector handles and stopped implementation receipts. Read/evaluate it when this
handoff is useful; it is not an always-loaded worker stage or prescribed tree.

Supply these existing bindings first:

- `task :: Task`, the incorporated assignment;
- `worker :: Forked (Outcome Candidate)`, the pending implementer;
- `reviewer :: Forked (Outcome ReviewDecision)`, whose prior attempt has settled;
- `reviewLabel :: RequestLabel`, a fresh request label;
- `onReview :: WorkSink (Outcome ReviewDecision)`, the local notification policy;
- `onStopped`, a pure message projection for a blocked/unavailable implementation;
- `owner :: ActorContextInfo`, captured in this Sol owner's turn.

For ordinary steering back to this Sol owner:

```haskell
import qualified Data.Text as T
owner <- actorContext
let onReview = notifyWork owner (workMessage reviewSummary)
let onStopped outcome = Just ("implementation: " <> either (T.pack . show) candidateSummary (settledValue outcome))
```

After evaluating the composition, keep `reviewDispatch` and `reviewBox`.
Query on the routed message: a parent snapshot does not flush descendants.
Model-free checks wait for the actual forwarded terminal event because they do
not run a model turn in response to native steering. The finite
route executes on its owner's effect stack, where reviewAgain may submit work.
It never awaits a model. The callback creates a persistent collector for that
attempt, registers its handle in reviewBox, and forwards typed progress/settlement
events there. Runtime response receipts remain attached to the terminal result.
`onReview` selects messages; partial evidence can stay local. An unavailable or
Blocked implementer goes to stoppedCandidates without issuing a review request.
That branch messages owner when onStopped selects a message and retains the
admission/failure receipt in
stoppedNotices; inspect reviewDispatch/reviewBox when intervention is needed.
It is not a successful review or permission to replace the implementer.

```haskell
flow <- Actor.call reviewBox (ReviewFlowSnapshot id)
inspectFull (reviewEvents flow, stoppedCandidates flow)
```

A finite route reports exceptional failure to its owner. If dispatch fails after
issuing an effect, inspect its retained receipt and the mailbox before retrying.
A callback-created handle is retained explicitly here; it is not magically added
to the caller's GHCi bindings. Normal typed actors intentionally have a smaller
effect profile than the owner's route callback.

After incorporating the verdict and placing unresolved questions/custody with
an owner, drain the recorded review collectors before their destination mailbox:

```haskell
traverse_ Actor.drainActor (reviewCollectors flow)
retiredReviews <- traverse Actor.awaitExit (reviewCollectors flow)
Actor.drainActor reviewBox
retiredFlow <- Actor.awaitExit reviewBox
```

Import `Data.Foldable (traverse_)` for that final snippet. Retain the exits and the
reviewer agent independently. The model-free automaticReview recipe exercises the
same composition with real Candidate/ReviewDecision values, exact source, an
available retained reviewer, and a blocked implementation that does not start review.
