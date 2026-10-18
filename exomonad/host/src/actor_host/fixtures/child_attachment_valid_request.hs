Right attachedChild <- spawnSubagent (FreshCtx "A valid child after the backend refusal.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "valid-after-backend-refusal" })
Right attachmentJob <- request @Int attachedChild (10 :: Int) (defaultRequestOptions { requestLabel = Just "request-after-backend-refusal" })
display (case agentBoundWorktree attachedChild of { Just tree -> Wt.worktreeId tree == Wt.worktreeId attachmentTree; _ -> False })
