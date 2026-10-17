{-# LANGUAGE QuasiQuotes #-}
let work = (task "implement-feature" "Implement the feature" ["feature.txt"] "Preserve the product gate" sourceHead)
      { planPath = "plans/current/feature.md", rationale = "Preserve the product boundary during preparation." }
workerAgent <- either (error . T.pack . show) pure =<< spawnSubagent (FreshCtx (taskContext work)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, workerQuestions) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work defaultRequestOptions
