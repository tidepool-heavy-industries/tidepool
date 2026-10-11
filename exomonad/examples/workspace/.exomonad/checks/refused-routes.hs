Right producer <- spawnSubagent (FreshCtx "Wait for a short work request.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "refused-worker" })
Right (producerRequest, updates) <- requestWithProgress @WorkProgress @Text producer "No allocation" defaultRequestOptions
beforeRefusal <- length . snapshotActors <$> snapshot
emptyRefusal <- traverse R.start (workDefinition [] (keepWork :: WorkSink Text))
blankRefusal <- traverse R.start (workDefinition [("  ", R.settlement producerRequest, R.progress updates)] (keepWork :: WorkSink Text))
duplicateRefusal <- traverse R.start (workDefinition [("same", R.settlement producerRequest, R.progress updates), ("same", R.settlement producerRequest, R.progress updates)] (keepWork :: WorkSink Text))
afterRefusal <- length . snapshotActors <$> snapshot
producerState <- pollResponse producerRequest
(case emptyRefusal of { Left NoSources -> True; _ -> False }) && (case blankRefusal of { Left BlankSourceName -> True; _ -> False }) && (case duplicateRefusal of { Left (DuplicateSourceName "same") -> True; _ -> False }) && beforeRefusal == afterRefusal && (case producerState of { ResponsePending _ -> True; _ -> False })
