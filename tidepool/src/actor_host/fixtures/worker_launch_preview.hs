let previewCampaign = "preview" :: CampaignLabel
let previewGroup = "workers" :: ForkGroupLabel
let previewLabel = "selected" :: BranchLabel
let previewTask = Task (batch previewCampaign previewGroup) "plans/component.md" "HEAD" "Implement one owned change" "Own the consumer boundary." ["feature.txt"] "Owning focused check passes" []
let proposed = withBranchGuidance "Read the selected plan before editing." (solTask previewLabel previewTask)
selectedPreview <- previewBranch proposed
let Right selectedDetails = selectedPreview
let Just selectedLaunch = previewLaunch selectedDetails
inspectFull (launchModel selectedLaunch == Just "gpt-5.6-sol", launchEffort selectedLaunch == Low, previewContext selectedDetails == SelectedContext, previewGuidance selectedDetails == Just "Read the selected plan before editing.", launchWorkspaceIdentity selectedLaunch == Just workspaceIdentity, launchModules selectedLaunch == "Shoal.Workspace" : workspaceModules, maybe False (`T.isPrefixOf` launchInstructions selectedLaunch) (workspacePrompt "task"), T.isInfixOf "Runtime policy" (launchInstructions selectedLaunch), T.length (launchBaseFingerprint selectedLaunch) == 64)
defaultPreview <- previewBranch (coding previewLabel projectHead previewTask)
inspectFull (fmap (fmap (\value -> (launchModel value, launchEffort value)) . previewLaunch) defaultPreview)
freshDefaultPreview <- previewBranch (withContext (selected taskContext) (coding previewLabel projectHead previewTask))
inspectFull (fmap (fmap (\value -> (launchModel value, launchEffort value)) . previewLaunch) freshDefaultPreview)
invalidPreview <- previewBranch (withLifetime SwarmOwned (coding previewLabel projectHead previewTask))
inspectFull (either id (const "unexpected admission") invalidPreview)
previewWorker <- unfold (batch previewCampaign previewGroup) (child @Candidate proposed)
