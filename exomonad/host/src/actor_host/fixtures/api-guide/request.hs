Right worker <- spawnSubagent (FreshCtx prompt) SameDir (defaultSpawnOptions spec)
Right pending <- request @Text worker input defaultRequestOptions
Right answer <- await (result pending)
display answer
