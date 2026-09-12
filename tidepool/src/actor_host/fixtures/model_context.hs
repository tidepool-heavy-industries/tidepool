let parentOnly = 41 :: Int
let campaign = "model-context" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let exactLabel = "exact" :: BranchLabel
let selectedLabel = "selected" :: BranchLabel
workers <- unfold (batch campaign wave) ((,) <$> child (withModel "gpt-5.6-sol" (coding @Text exactLabel projectHead ("exact" :: Text))) <*> child (withContext (selected (\task -> "Focused packet: " <> task)) (withModel "gpt-5.6-sol" (withEffort Medium (coding @Text selectedLabel projectHead ("selected" :: Text))))))
