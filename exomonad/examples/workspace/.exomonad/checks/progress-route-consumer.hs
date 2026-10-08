Right consumerAgent <- spawnSubagent (FreshCtx "Review the producer's questions and give feedback.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "consumer" })
Right consumer <- request @Text consumerAgent ([] :: Attention) defaultRequestOptions
