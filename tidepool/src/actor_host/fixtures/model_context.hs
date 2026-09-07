let parentOnly = 41 :: Int
let Right campaign = campaignLabel "model-context"
let Right wave = forkGroupLabel "workers"
let Right exactLabel = branchLabel "exact"
let Right selectedLabel = branchLabel "selected"
workers <- unfold (batch campaign wave) ((,) <$> child (withModel "gpt-5.6-sol" (coding @Text exactLabel projectHead ("exact" :: Text))) <*> child (withContext (selected (\task -> "Focused packet: " <> task)) (withModel "gpt-5.6-sol" (withEffort Medium (coding @Text selectedLabel projectHead ("selected" :: Text))))))
