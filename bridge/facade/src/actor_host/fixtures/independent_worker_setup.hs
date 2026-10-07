Right peerActor <- spawnSubagent (FreshCtx "First independent worker") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnLifetime = RunOwned, spawnModel = Just (Literal "gpt-6-sol"), spawnLabel = Just "worker" })
Right peer <- request @Text peerActor ("First independent worker" :: Text) defaultRequestOptions
