{-# LANGUAGE QuasiQuotes #-}
let work = (task "delivery-lead" "Deliver the feature" ["feature.txt"] "Retain the exact evidence" sourceHead)
      { planPath = "plans/current/feature.md", rationale = "Retain request ownership through automatic forwarding." }
Right leadAgent <- spawnSubagent (FreshCtx (taskContext work)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "executor", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right lead <- request @Candidate leadAgent work defaultRequestOptions
