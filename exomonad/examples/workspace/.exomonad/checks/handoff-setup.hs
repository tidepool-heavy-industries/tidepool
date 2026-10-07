{-# LANGUAGE QuasiQuotes #-}
let leftTask = (task "left" "Deliver the component" ["component source"] "read final source" sourceHead)
      { planPath = "plans/component.md", rationale = "Exercise typed subtree handoff" }
let rightTask = leftTask { taskName = "right" }
Right leftAgent <- spawnSubagent (FreshCtx (taskContext leftTask)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName leftTask) })
Right (left, leftProgress) <- requestWithProgress @WorkProgress @Delivery leftAgent leftTask
  (defaultRequestOptions { requestReporting = Silent })
Right rightAgent <- spawnSubagent (FreshCtx (taskContext rightTask)) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName rightTask) })
Right (right, rightProgress) <- requestWithProgress @WorkProgress @Delivery rightAgent rightTask
  (defaultRequestOptions { requestReporting = Silent })
