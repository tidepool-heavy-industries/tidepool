{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
import Tidepool.Worktree (workspaceFor)
let work = (task "implement" "Implement one component" ["review-flow.txt"] "Read exact committed source" sourceHead)
      { planPath = "plans/component.md", rationale = "Exercise effectful review routing" }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "luna", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, _updates) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
Right coordinatorWorkspace <- workspaceFor coordinatorTree
flow <- R.start (R.withWorkspace coordinatorWorkspace
  (reviewFlowWith me work defaultReviewFlowPolicy worker
    (\_ -> pure (ReviewRouteResult
      (EscalateReview "owner must decide this scope change") DeterministicRoute))))
