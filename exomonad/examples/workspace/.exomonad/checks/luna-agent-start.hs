Right compilerScout <- spawnSubagent
  (FreshCtx "Inspect the compiler boundary and return one concise finding.")
  SameDir
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "luna")
    , spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task")
    , spawnLabel = Just "compiler-scout"
    })
Right lookupScout <- spawnSubagent
  (FreshCtx "Inspect the lookup API and return one concise finding.")
  SameDir
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "luna")
    , spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task")
    , spawnLabel = Just "lookup-scout"
    })
Right compilerRequestHandle <- request @Text compilerScout
  "Summarize the named compiler boundary in one sentence."
  defaultRequestOptions
Right lookupRequestHandle <- request @Text lookupScout
  "Summarize the public lookup contract in one sentence."
  defaultRequestOptions
