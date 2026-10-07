{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
-- Supply routeCriteria for this task. Exact-source Repair uses Jev only when
-- criteria are present; Accepted and missing-criteria escalation are local.
let work = (task "implement" "Implement one component" ["review-flow.txt"] "Read exact committed source" sourceHead)
      { planPath = "plans/component.md", rationale = "Review its exact committed candidate" }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "luna", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, _updates) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
Right coordinatorTree <- createWorktree
  (fromRef (GitRef (renderGitOid sourceHead)) coordinatorName)
let policy = defaultReviewFlowPolicy
      { flowRepairLimit = 1
      , flowEscalationCriteria = routeCriteria }
flow <- R.start (R.withWorktree (worktreeId coordinatorTree)
  (reviewFlowWith me work policy worker semanticReviewChoice))
initialSnapshot <- R.call (reviewSnapshot (R.client flow)) ()
pendingCleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce
inspectFull (show (flowStage initialSnapshot, pendingCleanup))
