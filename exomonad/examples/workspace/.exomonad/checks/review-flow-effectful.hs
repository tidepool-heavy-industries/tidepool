{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
let campaign = campaignName :: CampaignLabel
let task = Task (batch campaign "component") "plans/component.md" sourceHead
      "Implement one component" "Exercise effectful review routing"
      ["review-flow.txt"] "Read exact committed source" []
(worker, _updates) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (coding (atRef (GitRef (renderGitOid sourceHead))) (assignment [label|implement|] task)))
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
flow <- R.start (R.withWorktree (worktreeId coordinatorTree)
  (reviewFlowWith me task defaultReviewFlowPolicy worker
    (\_ -> pure (ReviewRouteResult
      (EscalateReview "owner must decide this scope change") DeterministicRoute))))
initialRoute <- R.forwardResult worker (firstCandidate (R.client flow))
