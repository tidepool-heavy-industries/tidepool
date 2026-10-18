import qualified Data.Text as FixtureText

attachedAdmission <- spawnSubagent (FreshCtx "A valid child after the model grant refusal.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "valid-after-model-grant-refusal" })
attachedChild <- case attachedAdmission of { Left refusal -> error (FixtureText.pack (show refusal)); Right actor -> pure actor }
Right attachmentJob <- request @Int attachedChild (10 :: Int) (defaultRequestOptions { requestLabel = Just "request-after-model-grant-refusal" })
display (case agentBoundWorktree attachedChild of { Just tree -> Wt.worktreeId tree == Wt.worktreeId attachmentTree; _ -> False })
