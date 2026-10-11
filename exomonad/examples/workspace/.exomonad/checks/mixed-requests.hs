data MixedResult = TextResult Text | NumberResult Int deriving (Show, Eq)
Right textAgent <- spawnSubagent (FreshCtx "Inspect the text source.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "text-worker" })
Right (textRequest, textProgress) <- requestWithProgress @Bool @Text textAgent "Inspect text" defaultRequestOptions
Right numberAgent <- spawnSubagent (FreshCtx "Inspect the number source.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "number-worker" })
Right (numberRequest, numberProgress) <- requestWithProgress @Text @Int numberAgent "Inspect numbers" defaultRequestOptions
let textSource = projectWorkSource "text" (const (WorkProgress [] [])) TextResult (R.settlement textRequest) (R.progress textProgress)
let numberSource = projectWorkSource "number" (const (WorkProgress [] [])) NumberResult (R.settlement numberRequest) (R.progress numberProgress)
Right buildCollection <- pure (workSourcesDefinition [textSource, numberSource])
collection <- R.start (buildCollection keepWork)
