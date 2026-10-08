Right worker <- spawnSubagent (FreshCtx "Reply to the ping when it arrives.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "worker" })
Right response <- request @Text worker "reply ping" defaultRequestOptions
sendMessage worker "ping"
