Right producer <- spawnSubagent (FreshCtx "Wait for a short work request.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just "refused-worker" })
Right (producerRequest, updates) <- requestWithProgress @WorkProgress @Text producer "No allocation" defaultRequestOptions
beforeRefusal <- length . snapshotActors <$> snapshot
emptyRefusal <- followWork [] (keepWork :: WorkSink Text)
blankRefusal <- followWork [("  ", producerRequest, updates)] (keepWork :: WorkSink Text)
duplicateRefusal <- followWork [("same", producerRequest, updates), ("same", producerRequest, updates)] (keepWork :: WorkSink Text)
afterRefusal <- length . snapshotActors <$> snapshot
producerState <- pollResponse producerRequest
(case emptyRefusal of { Left NoSources -> True; _ -> False }) && (case blankRefusal of { Left BlankSourceName -> True; _ -> False }) && (case duplicateRefusal of { Left (DuplicateSourceName "same") -> True; _ -> False }) && beforeRefusal == afterRefusal && (case producerState of { ResponsePending _ -> True; _ -> False })
