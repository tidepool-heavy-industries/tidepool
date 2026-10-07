{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Effects.Core (GitRef (..))
let work = (task "implement" "Implement one component" ["review-flow.txt"] "Read exact committed source" sourceHead)
      { planPath = "plans/component.md", rationale = "Exercise fresh review and bounded repair" }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work))
  (ForkWorktree (atRef (GitRef (renderGitOid sourceHead))))
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "luna", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, _updates) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
