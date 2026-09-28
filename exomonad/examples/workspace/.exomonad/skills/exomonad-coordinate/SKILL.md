---
name: exomonad-coordinate
description: Split a shared Exomonad Task into children, then route and inspect progress and replies with Project.Routing actors.
---

For a new batch, `unfoldWork group [workChild name branch, ...] sink` composes
admission and this collector. It returns `WorkBatch` with the original typed
handles in `batchMembers` and collector in `batchRouter`. All branches in one
batch share their result type; use separate batches for different types.
`finishWorkBatch` refuses while any result is pending. It drains the collector,
not the children. The compiled `RECURSIVE-WORK.md` example shows recursive owners
and a later batch without a preauthored graph.

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

In the parent, retain `localBatch :: WorkBatch (Outcome Candidate)` from the
fork skill and use its existing collector. The handler receives changes without
rearming. `WorkSink` is a named value, so
you can retain and compose `notifyWork` results across notebook cells. For a
custom callback, construct `WorkSink (\event -> ...)`; its effects remain owned
by the collector:

```haskell
import qualified Tidepool.Actor as Actor
let router = batchRouter localBatch
state <- readWork router
inspectFull (workSnapshotSummary candidateOutcomeSummary state)
```

`workSnapshotSummary :: (value -> Text) -> WorkState value -> Text` shows source
status, candidate commits, question keys/sources and final results. It does not
consume state. Inspect `collectedWork state` for full evidence or `workNotices state`
for delivery receipts when needed. Call it for a decision, not as a polling ritual.
`withCheckpoints (workMessage candidateOutcomeSummary)` additionally notifies on candidate
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

When the integration cycle is finished, `finishWorkBatch localBatch` drains the collector
and returns its retained final state. Keep it through repairs; a model turn ending
is not a reason to retire it. `releaseGroup groupHandle` separately asks the existing
cleanup owner to release workers you no longer need, retaining uncertain members:

```haskell
retiredWork <- finishWorkBatch localBatch
let Just group = forkGroupHandle worker
released <- releaseGroup group
inspectFull released
```

Retain the cleanup receipt; a blocked step leaves that work with its current owner.
Do not turn it into a stop/retry loop.

For custom typed joins or automatic request continuations, load
`exomonad-define-actors`. Routine routing stays in Haskell; wake the local owner only
for engineering decisions, actionable failures or integration work.
`Project.RebaseRouter`'s `rebaseRouter` actor watches an integration worktree and
each child's own commits and nudges a child with `sendMessage` when the child is
behind and the advance overlaps its committed paths, so routine rebase prompting
does not need a parent turn either.

## A ready frontier from one shared contract

`unfoldWork` admits the children and attaches the collector in one cell. Each
child still scaffolds/delegates or justifies a terminal leaf.

Start from the accepted `Task` and a checked shared source. Bind the shared plan
once. An ordinary local selector can make short, disjoint assignments without
a new workspace workstream registry. This cell assumes `baseline :: GitOid` is bound
to the source being split. Both branches use `lunaTaskFrom` with fresh context
selected from their Tasks and the `currentCheckout` seed of the local owner. Put the shared
decisions and seam contracts they need in those Tasks. `lunaTaskFrom` selects
the cheap `luna` alias with the effort you pass; `solTaskFrom` uses inherited
context by default; crossing from Luna requires `withContext (selected taskContext)`.
Routine subtree decisions and integration stay
with the Luna owner; focused Luna descendants should inherit useful scaffold context.

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

sliceAcceptance :: WorkSlice -> Text
sliceAcceptance InterfaceSlice = "The committed interface contract has usable semantics and focused interface tests pass"
sliceAcceptance ConsumerSlice = "The consumer uses the committed interface and its focused tests pass; the parent owns combined product acceptance"

let group = batch "corpus" "fanout"
let shared = (task [label|feature|] "Deliver the feature through its real consumer" [] "Integrated behavior and focused checks" baseline)
      { taskGroup = group
      , planPath = "plans/feature.md"
      , rationale = "The interface and consumer share one accepted contract"
      }
let sliceTask slice = shared
      { obligation = sliceName slice <> ": implement and check the assigned slice"
      , ownedPaths = slicePaths slice
      , acceptance = sliceAcceptance slice
      }
let sliceBranch slice = lunaTaskFrom (sliceLabel slice) Medium currentCheckout (sliceTask slice)
localBatch <- unfoldWork group
  [ workChild "interface" (sliceBranch InterfaceSlice)
  , workChild "consumer" (sliceBranch ConsumerSlice)
  ] (notifyWork me (withCheckpoints (workMessage candidateOutcomeSummary)))
let [(_, interface, interfaceProgress), (_, consumer, consumerProgress)] = batchMembers localBatch
```

Admission creates two pending obligations and their persistent collector. End
that cell promptly. On an actionable notice, read the same collector:

```haskell
let router = batchRouter localBatch
state <- readWork router
inspectFull (workSnapshotSummary candidateOutcomeSummary state)
```

`unfoldWork` hands settlement delivery to the collector, suppressing each child's
duplicate direct wake. Routine progress stays in `state`; a question, unavailable result,
terminal result, or candidate checkpoint wakes the owner. On wake, inspect the
relevant candidate and source receipt before incorporating it. Mark handled
evidence with `incorporatedWork`, drain the router after the wave, and release
finished children. A third repair at one boundary, or eight model rounds without
a fork or candidate checkpoint, triggers a work-split reassessment. Commit a
changed allocation or ask the parent for authority; continue locally if the
remaining work is truly bounded.
