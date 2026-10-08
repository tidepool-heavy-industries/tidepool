Right attachedChild <- spawnSubagent (FreshCtx "A valid child after the local attachment refusal.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "valid-after-alias-child", spawnModel = Just (Literal "test-model") })
Right attachmentJob <- request @Int attachedChild (10 :: Int) (defaultRequestOptions { requestLabel = Just "request-after-alias-refusal" })
display (case agentBoundWorktree attachedChild of { Just tree -> Wt.worktreeId tree == Wt.worktreeId attachmentTree; _ -> False })
