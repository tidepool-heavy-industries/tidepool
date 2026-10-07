{-# LANGUAGE QuasiQuotes #-}
Right leftAgent <- spawnSubagent (FreshCtx "Inspect the left side and report your findings.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "left" })
Right (left, leftProgress) <- requestWithProgress @WorkProgress @Text leftAgent "left" defaultRequestOptions
Right rightAgent <- spawnSubagent (FreshCtx "Inspect the right side and report your findings.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "right" })
Right (right, rightProgress) <- requestWithProgress @WorkProgress @Text rightAgent "right" defaultRequestOptions
