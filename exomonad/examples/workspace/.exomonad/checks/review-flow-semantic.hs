{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
import Tidepool.Worktree (workspaceFor)
let work = (task "implement" "Implement one component" ["review-flow.txt"] "Read exact committed source" sourceHead)
      { planPath = "plans/component.md", rationale = "Exercise exact-source review and semantic repair routing" }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "luna", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, _updates) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
Right coordinatorWorkspace <- workspaceFor coordinatorTree
let policy = defaultReviewFlowPolicy
      { flowRepairLimit = limit
      , flowSourcePlan = sourcePlan
      , flowEscalationCriteria = ["The findings require a path outside review-flow.txt or a change to the assigned acceptance."]
      }
flow <- R.start (R.withWorkspace coordinatorWorkspace
  (reviewFlowWith me work policy worker semanticReviewChoice))
