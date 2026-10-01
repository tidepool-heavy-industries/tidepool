{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
-- Supply routeCriteria for this task. Exact-source Repair uses Jev only when
-- criteria are present; Accepted and missing-criteria escalation are local.
let campaign = campaignName :: CampaignLabel
let task = Task (batch campaign "component") "plans/component.md" sourceHead
      "Implement one component" "Review its exact committed candidate"
      ["review-flow.txt"] "Read exact committed source" []
(worker, _updates) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (withLifetime ActorOwned $ withContext (selected taskContext) $ coding (atRef (GitRef (renderGitOid sourceHead))) (assignment [label|implement|] task)))
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
let policy = defaultReviewFlowPolicy
      { flowRepairLimit = 1
      , flowEscalationCriteria = routeCriteria }
flow <- R.start (R.withWorktree (worktreeId coordinatorTree)
  (reviewFlowWith me task policy worker semanticReviewChoice))
initialSnapshot <- R.call (reviewSnapshot (R.client flow)) ()
pendingCleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce
inspectFull (show (flowStage initialSnapshot, pendingCleanup))
