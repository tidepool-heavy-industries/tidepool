Right worker <- spawnSubagent (FreshCtx prompt) SameDir (defaultSpawnOptions currentModel spec)
Right pending <- request @Text worker input defaultRequestOptions
Right answer <- await (result pending)
display answer
