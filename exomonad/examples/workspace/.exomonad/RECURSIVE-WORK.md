# Recursive work: the execution workflow

Every execution owner uses the same cycle:

**scaffold → admit a ready parallel batch → integrate checked results → repeat.**

The Sol root owns cross-component decisions. Luna component owners repeat the
cycle through subcomponent owners to microtask leaves. Aim for at least three
Luna implementation levels on average, with real parallel work at each useful
frontier. Before substantial direct implementation, explain why the remaining
assignment is a terminal leaf. A reviewer is not an implementation level.

Scaffold only what the next children need: shared types, module wiring, exact
source, owned paths, local gates and an integration owner. Reuse existing source.
The child has its own narrower acceptance; the parent still owes the combined
gate. Commit the shared boundary, admit independent work together, and do local
integration work while it runs. Do not author a global graph of guessed steps.

## Admit and observe a local batch

`lunaLead label effort task` returns a branch requesting `Delivery`, using the
configured `lead` instructions. `lunaTask` is polymorphic: code changes can return
`Outcome Candidate`; investigation can return `Outcome Text`; other contracts
keep their actual type. Nothing requires a fabricated commit.

`unfoldWork group branches sink` composes `unfold` and the existing work collector.
It silences each branch's duplicate settlement notification, returns the original
typed response/progress handles in `batchMembers`, and the collector in
`batchRouter`. Each batch has one result type; independent batches can differ.
Do not await new children inside their admission cell.

The runnable cells in [checks/recursive-batch.hs](checks/recursive-batch.hs) admit
two independent findings tasks. [Project.RecursiveWorkChecks.nestedBatches](Project/RecursiveWorkChecks.hs)
uses those same cells at three descendant levels, then admits a second local
batch after the first results arrive. It demonstrates the mechanics without
claiming findings are implementation work or measuring model performance.
Replace the tasks with the actual ready frontier and use `lunaLead` for component
owners. Choose selected context across model tiers; focused Luna descendants should
use `withContext inherited` to reuse the scaffold reasoning. Later source or decisions need explicit delivery.

On an actionable notice, read the collector once and inspect relevant original
receipts. A candidate, reply, accepted review, integrated source and released
resources are different facts. `finishWorkBatch` refuses while any original
result remains pending; a successful finish drains the collector, not the workers.
Make a brief kaizen answer part of each child's handoff and read it before
retirement. After reviewing, integrating and checking the local result, finish
the collector and release the completed group with the cleanup skill. One pending
member blocks the entire group before any stop; retain that group for named work.
Separately admitted sibling groups can be released independently. Keep the cleanup
receipt; `StoppedReleasing` still needs its later host notice.

## Checked review, repair and integration

For a separate implementer whose original response produces `Outcome Candidate`,
use `startReviewFlow owner task policy response checks`. The return is
`Either Text ReviewRun`; it retains the flow actor, initial forwarding route and
check worktree handle. The caller supplies focused `PlanCheck` values rather than
reconstructing command/evidence logic at every review.

ReviewFlow validates the original response's exact source, runs counted checks,
admits an exact-source reviewer, and routes within-contract repair to the retained
implementer under one repair budget. Unknown check evidence and scope uncertainty
stop for the owner. Reviewer and repair questions are collected while their
requests stay pending; the local owner receives question changes without a second
settlement notice. Each collector drains on its original result and retains its
exit. The automatic reviewer inspects source and supplied check evidence; the
flow owns executed checks. On a question notice, `reviewSnapshot` retains the
active `flowReviewerCollectors` and `flowRepairCollectors`; use `readWork` for
the full question and the corresponding original response to identify its actor.
Settled collector exits remain in `flowReviewerCollected` and `flowRepairCollected`.
Set explicit escalation criteria for semantic repair routing.
`startReviewFlowWith` accepts a project-specific bounded routing function using
the same machinery. It cannot upgrade a repair verdict into acceptance.

The executed setup is [checks/checked-review-setup.hs](checks/checked-review-setup.hs).
Its recipes cover green evidence, check repair, zero selection, missing evidence,
source mismatch, escalation, and publication. For local automatic integration,
set `flowIntegration` to an existing `Project.Merge.MergeTarget`. That actor
serializes checked publication; `ReviewIntegrated` retains its actual result.
A failed integration retains its source/evidence and returns to the owner.
Leave integration unset when the owner will perform that step itself.

Never send repair to yourself while your Delivery is waiting for that review.
Keep leaf review focused on its change; component review checks joins and the
combined acceptance instead of repeating every descendant's review. For manual
review of a revised candidate, `requestReview uniqueLabel revisedRequest` admits
the exact checkout while preserving the supplied review basis. Merely sending a
new request to a retained reviewer does not move that actor's checkout.

Read the terminal flow and retain interviews before `reviewCleanup`; preserve
its actual stop outcomes, then finish the flow. The returned check worktree is
explicit retained ownership, not evidence of resource release.

## Relay existing decisions without parent bookkeeping

Use `unfoldAnsweredWork owner group [(name, task, branchConstructor), ...] sink`
to admit a batch and attach answers in one call. For example, a constructor can
be `lunaLead childLabel Medium`; the helper supplies its exact Task and binds the
recipient from the actual response. For an existing admission,
`startDecisionAnswers owner targets` attaches the decisions already authorized. Each `AnswerTarget` binds the collector's source name,
recipient and exact Task. Decorate its normal sink with `withDecisionAnswers`.
Jev selects only a supplied original decision. The receiver gets that exact
decision and evidence; it must still check applicability and incorporation.

The deterministic guard requires the question's plan/source and every supplied
decision's source to match the Task. Changed source needs a newly authorized set.
At most eight decisions and 6,000 encoded state characters enter a judgment;
an actor retains at most 32 distinct episodes. Repeated identical questions reuse
the original receipt and never retry an uncertain send. An admitted notification
suppresses that question's immediate parent notice; it proves neither reading nor
incorporation. Unknown, conflicting, oversized,
exhausted or failed cases preserve it. Full questions remain in the collector.

The answer actor is part of that batch's routing lifetime. Disable it before a
decision changes; finish the collector before finishing the answer actor. This
does not amend a Task or authorize a rebase. Shared-design changes still go to
the parent. [DecisionAnswerChecks.routing](Project/DecisionAnswerChecks.hs) exercises
the guards and retained original decisions with a deterministic injected chooser;
live semantic probes are a separate gate.

## Judge the workflow by delivered work

Record useful implementation depth and overlap, time to the first useful fork,
dependency waiting, source corrections, reviewer findings, parent relay rounds,
and elapsed time to checked delivery. Three levels of serialized forwarding do
not satisfy the purpose. The experiment is parallel engineering with less
bookkeeping and strong review, not an actor-count target.
