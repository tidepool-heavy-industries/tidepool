{-# LANGUAGE QuasiQuotes #-}
let work = (task "candidate" "Submit a candidate" ["candidate source"] "read exact source" sourceHead)
      { planPath = "plans/component.md", rationale = "Check the terminal source receipt" }
Right workerAgent <- spawnSubagent (FreshCtx (taskContext work)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnModel = Just (Alias "luna"), spawnEffort = Just Medium
    , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
Right (worker, updates) <- requestWithProgress @WorkProgress @(Outcome Candidate) workerAgent work
  (defaultRequestOptions { requestReporting = Silent })
Right readiness <- followWork [("candidate", worker, updates)] (notifyReviewReady me)
