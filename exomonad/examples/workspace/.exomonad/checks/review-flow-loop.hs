{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
let campaign = campaignName :: CampaignLabel
let task = Task (batch campaign "component") "plans/component.md" sourceHead
      "Implement one component" "Exercise fresh review and bounded repair"
      ["review-flow.txt"] "Read exact committed source" []
(worker, _updates) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (coding (atRef (GitRef (renderGitOid sourceHead))) (assignment [label|implement|] task)))
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
flow <- R.start (R.withWorktree (worktreeId coordinatorTree)
  (reviewFlow me task
    (defaultReviewFlowPolicy { flowRepairLimit = limit, flowSourcePlan = sourcePlan }) worker))
initialRoute <- R.forwardResult worker (firstCandidate (R.client flow))
