{-# LANGUAGE QuasiQuotes #-}
let destination = respond
Right candidateAgent <- spawnSubagent (FreshCtx (taskContext sessionInput)) (ForkWorktree currentCheckout)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just "executor", spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "feature" })
Right candidate <- request @Candidate candidateAgent sessionInput defaultRequestOptions
forwarding <- route (result candidate) destination
