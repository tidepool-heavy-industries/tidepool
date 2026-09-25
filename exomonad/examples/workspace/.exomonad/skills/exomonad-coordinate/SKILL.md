---
name: exomonad-coordinate
description: Split a shared Exomonad Task into children, then route and inspect progress and replies with Project.Routing actors.
---

For a worker launched with `childWithProgress @WorkProgress`, its current request
installs `reportProgress`. Given `candidate :: Candidate`, publish useful evidence
without ending the request:

```haskell
let update = WorkProgress [candidate] []
reportProgress update
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
`exomonad-define-actors`. Routine routing stays in Haskell; wake the owning Sol only
for engineering decisions, actionable failures or integration work.
`Project.RebaseRouter`'s `rebaseRouter` actor watches an integration worktree and
each child's own commits and nudges a child with `sendMessage` when the child is
behind and the advance overlaps its committed paths, so routine rebase prompting
does not need a parent turn either.

## Split one shared Task

Start from the accepted `Task` and a checked shared source. Bind the shared plan
once. An ordinary local selector can make short, disjoint assignments without
a new workspace workstream registry. This cell assumes `baseline :: GitOid` is bound
to the source being split. Both branches use `lunaTaskFrom` with fresh context
selected from their Tasks and the `projectHead` checkout seed. Put the shared
decisions and seam contracts they need in those Tasks. `lunaTaskFrom` selects
the cheap `luna` alias with the effort you pass; `solTaskFrom` uses inherited
context for Sol children that own design or integration.

```haskell
data WorkSlice = InterfaceSlice | ConsumerSlice deriving (Show, Eq)

sliceName :: WorkSlice -> Text
sliceName InterfaceSlice = "interface"
sliceName ConsumerSlice = "consumer"

sliceLabel :: WorkSlice -> Label
sliceLabel InterfaceSlice = [label|interface|]
sliceLabel ConsumerSlice = [label|consumer|]

slicePaths :: WorkSlice -> [Text]
slicePaths InterfaceSlice = ["src/interface.rs", "tests/interface.rs"]
slicePaths ConsumerSlice = ["src/consumer.rs", "tests/consumer.rs"]

let group = batch "corpus" "fanout"
let shared = (task [label|feature|] "Deliver the feature through its real consumer" [] "Integrated behavior and focused checks" baseline)
      { taskGroup = group
      , planPath = "plans/feature.md"
      , rationale = "The interface and consumer share one accepted contract"
      }
let sliceTask slice = shared
      { obligation = sliceName slice <> ": implement and check the assigned slice"
      , ownedPaths = slicePaths slice
      }
let sliceBranch slice = withReport Silent (lunaTaskFrom (sliceLabel slice) Medium projectHead (sliceTask slice))
((interface, interfaceProgress), (consumer, consumerProgress)) <- unfold group $
  (,) <$> childWithProgress @WorkProgress @(Outcome Candidate) (sliceBranch InterfaceSlice)
      <*> childWithProgress @WorkProgress @(Outcome Candidate) (sliceBranch ConsumerSlice)
```

Admission creates two pending obligations; it does not join their results. End
that cell promptly. In the next cell, attach one persistent selective collector:

```haskell
router <- followWork
  [ ("interface", interface, interfaceProgress)
  , ("consumer", consumer, consumerProgress)
  ] (notifyWork me (withCheckpoints (workMessage candidateSummary)))
state <- readWork router
inspectFull (workSnapshotSummary candidateSummary state)
```

`withReport Silent` hands settlement delivery to the record actor; without it
every child would also wake you directly. Routine progress stays in `state`; a question, unavailable result,
terminal result, or candidate checkpoint wakes the owner. On wake, inspect the
relevant candidate and source receipt before incorporating it. Mark handled
evidence with `incorporatedWork`, drain the router after the wave, and release
finished children. A third repair at one boundary, or eight model rounds without
a fork or candidate checkpoint, triggers a work-split reassessment. Commit a
changed allocation or ask the parent for authority; continue locally if the
remaining work is truly bounded.
