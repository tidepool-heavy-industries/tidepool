Right attachedChild <- spawnSubagent (FreshCtx "A valid child after the model grant refusal.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "valid-after-model-grant-refusal" })
Right attachmentJob <- request @Int attachedChild (10 :: Int) (defaultRequestOptions { requestLabel = Just "request-after-model-grant-refusal" })
display (case agentBoundWorktree attachedChild of { Just tree -> Wt.worktreeId tree == Wt.worktreeId attachmentTree; _ -> False })
