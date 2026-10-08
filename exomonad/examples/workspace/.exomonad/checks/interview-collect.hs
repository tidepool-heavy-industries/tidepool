{-# LANGUAGE QuasiQuotes #-}
let question = Question "source-choice" (DesignQuestion
      "plans/component.md" sourceHead "Choose a source" [] [] ["implementation"])
Right expertAgent <- spawnSubagent (FreshCtx (questionFinding (questionDetails question))) (ForkWorktree projectHead)
  ((defaultSpawnOptions workspaceAgentSpec)
    { spawnInstructions = Just (projectPrompt "specialist"), spawnLabel = Just "source-expert" })
Right (expert, _updates) <- requestWithProgress @WorkProgress @DesignAnswer expertAgent
  (questionDetails question) defaultRequestOptions
let interviewItems = [AwaitAnswer question expert]
