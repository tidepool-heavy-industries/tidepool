Right observerActor <- spawnSubagent (FreshCtx "Retain the exact peer handle for followup") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnLifetime = RunOwned, spawnModel = Just (Literal "gpt-6-sol"), spawnLabel = Just "observer" })
Right peerObserver <- request @Text observerActor (responseActor peer) defaultRequestOptions
