# Recursive work: the execution workflow

Every execution owner uses the same cycle:

**scaffold → admit a ready parallel batch → integrate checked results → repeat.**

The Sol root owns cross-component choices. Luna component owners repeat the cycle
through subcomponents to justified microtask leaves. Useful implementation depth
requires shared boundaries and independent work; reviewers or forwarding nodes
are not implementation levels. Explain a terminal leaf before substantial direct
implementation.

Scaffold only what the next children need: shared types, consumer wiring, exact
source, owned paths, local gates and an integration owner. Commit that boundary,
admit independent work together and continue local integration. The child owes
its own gate; the parent still owes combined acceptance. A later batch uses its
actual new source rather than a prewritten global graph.

## Admit and collect a local batch

`Project.Work.lunaLead` requests `Delivery`; `lunaTask` preserves the actual result
type, including no-change findings. Model aliases, effort and prompt selection
are project policy. Helpers select focused Task context; a descendant reusing a
scaffold can choose `withContext (fromCheckpoint captured)` explicitly.

`Exomonad.Contrib.Routing.unfoldWorkBatch` admits an applicative plan and returns
`Either BatchFailure (RoutedBatch handles event)`. Its original heterogeneous
handle product remains in `routedMembers`; results map into an authored event
sum. `unfoldWork` is the homogeneous-list convenience returning a `WorkBatch`,
with original response/progress handles in `batchMembers` and collector in
`batchRouter`. Preflight checks the full plan before allocation. Optional
observer refusal or delivery failure is retained without breaking collection.

Default child lifetime is `InvocationOwned`. Await immediate children in the
same invocation for an ordinary dependent program. Decorate branches with
`withLifetime ActorOwned` when model decisions or work across turns participate.
Returning handles, registering a watch or ending a model response does not
transfer lifetime. Deferred admission requires explicit persistent lifetime and
must return before its children start. See [WORKBENCH.md](WORKBENCH.md).

The source cells in [checks/recursive-batch.hs](checks/recursive-batch.hs) and
[Project.RecursiveWorkChecks.nestedBatches](Project/RecursiveWorkChecks.hs) exercise
successive local batches through nested owners. They test mechanics and preserve
failure observations; they do not measure model performance or establish live
implementation acceptance.

On an actionable notice, read the collector and relevant original receipts.
`acknowledgeWork` marks inspected publications, never verified incorporation.
`finishWorkBatch` refuses while original results remain pending and drains the
collector after settlement. It does not retire workers. Read each child's brief
kaizen handoff, review exact source, integrate and verify the resulting revision.
Release completed groups with the cleanup skill and retain actual stop outcomes;
a pending group member blocks the group before any stop. Separately admitted
groups can be released independently. `StoppedReleasing` needs its later notice.

## Checked review, repair and integration

For a separate implementer, `startReviewFlow owner task policy response checks`
returns `Either Text ReviewRun`. It subscribes to the original worker directly
and retains its flow and check worktree. `Exomonad.Contrib.ReviewFlow` owns the
continuation; `Project.ReviewPolicy` supplies reviewer instructions, placement,
check/review policy and optional bounded semantic repair routing.

The flow validates the exact submission, executes counted checks and admits a
reviewer at that source. It retains original reviewer and repair handles, progress,
questions and typed attachment refusals. `reviewSnapshot` exposes the retained
state. Known within-contract repairs use one budget and a separate implementer;
unknown evidence, scope uncertainty or failed admission stops for the owner.
Never queue repair behind your own delivery while it waits for review.

Authored `reportedChecks` and `reviewNotes` remain claims. Counted execution,
original exact review proof and integration checks remain separate evidence.
An automatic inspection reviewer consumes source and supplied check evidence;
choose a coding reviewer when it must execute checks. Component review examines
joins and combined acceptance rather than repeating every descendant's review.

[checks/checked-review-setup.hs](checks/checked-review-setup.hs) supplies model-free
recipes for green evidence, repair, zero matches, missing evidence, source mismatch,
escalation and publication. These examples require execution on the integrated
revision before reporting them as passing. Optional `flowIntegration` uses an
existing `Exomonad.Contrib.Merge.MergeTarget` for serialized checked publication.
A failed integration retains source and evidence; leave integration unset when
the owner will integrate directly.

A revised manual review uses `requestReview uniqueLabel revisedRequest` for an
exact-source reviewer. Merely requesting another commit from a retained reviewer
does not move its checkout. Read terminal flow evidence and interviews before
`reviewCleanup`, retain its actual stop outcomes, then finish the flow. The check
worktree remains explicitly owned until its cleanup path releases it.

## Relay current decisions

`Project.DecisionAnswers` can relay already authorized decisions from exact
Tasks and source. Its normal sink retains uncertain, conflicting, stale, oversized
or failed cases for the owner. Notification admission proves neither reading nor
incorporation. Keep source-bearing decisions current; disable the answer actor
before changing them and drain its collector before finishing it. It does not
amend tasks, authorize rebases or replace shared-design decisions.

## Evaluate delivered work

Record useful implementation depth and overlap, first useful fork, dependency
waiting, source corrections, reviewer findings, relay rounds and time to checked
delivery. Structural simplification is distinct from a measured speedup. Live
model trials, adapter acceptance and publication remain their own gates.
