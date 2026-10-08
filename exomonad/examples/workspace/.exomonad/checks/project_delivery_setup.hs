{-# LANGUAGE QuasiQuotes #-}
let work = (task "implement-feature" "Implement the feature" ["feature.txt"] "Preserve the product gate" sourceHead)
      { planPath = "plans/current/feature.md", rationale = "Preserve the product boundary during preparation." }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "executor"), spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, workerQuestions) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
