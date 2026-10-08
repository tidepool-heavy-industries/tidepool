{-# LANGUAGE QuasiQuotes #-}
Right producerAgent <- spawnSubagent (FreshCtx "Inspect the contract and report questions as they arise.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "producer" })
Right (producer, updates) <- requestWithProgress @WorkProgress @Text producerAgent "inspect contract"
  (defaultRequestOptions { requestReporting = Silent })
