Right compilerScout <- spawnSubagent
  (FreshCtx "Inspect the compiler boundary and return one concise finding.")
  SameDir
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnInstructions = Just (projectPrompt "task")
    , spawnLabel = Just "compiler-scout"
    })
Right introspectionScout <- spawnSubagent
  (FreshCtx "Inspect the introspection API and return one concise finding.")
  SameDir
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnInstructions = Just (projectPrompt "task")
    , spawnLabel = Just "introspection-scout"
    })
Right compilerRequestHandle <- request @Text compilerScout
  "Summarize the named compiler boundary in one sentence."
  defaultRequestOptions
Right introspectionRequestHandle <- request @Text introspectionScout
  "Summarize the public introspection contract in one sentence."
  defaultRequestOptions
