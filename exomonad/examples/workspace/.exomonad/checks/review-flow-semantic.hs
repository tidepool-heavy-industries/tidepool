{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
let campaign = campaignName :: CampaignLabel
let task = Task (batch campaign "component") "plans/component.md" sourceHead
      "Implement one component" "Exercise exact-source review and semantic repair routing"
      ["review-flow.txt"] "Read exact committed source" []
(worker, _updates) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (withLifetime ActorOwned $ withContext (selected taskContext) $ coding (atRef (GitRef (renderGitOid sourceHead))) (assignment [label|implement|] task)))
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
let policy = defaultReviewFlowPolicy
      { flowRepairLimit = limit
      , flowSourcePlan = sourcePlan
      , flowEscalationCriteria = ["The findings require a path outside review-flow.txt or a change to the assigned acceptance."]
      }
flow <- R.start (R.withWorktree (worktreeId coordinatorTree)
  (reviewFlowWith me task policy worker semanticReviewChoice))
