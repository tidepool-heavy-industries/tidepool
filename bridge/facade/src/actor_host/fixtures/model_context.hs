let parentOnly = 41 :: Int
let campaign = "model-context" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let exactLabel = [label|exact|]
let selectedLabel = [label|selected|]
let exactBranch = withModel (Literal "gpt-6-sol") (coding @Text projectHead (assignment exactLabel ("exact" :: Text)))
let selectedBranch = withContext (selected (\task -> "Focused packet: " <> task)) (withModel (Literal "gpt-6-sol") (withEffort Medium (coding @Text projectHead (assignment selectedLabel ("selected" :: Text)))))
workers <- unfold (batch campaign wave) ((,) <$> child exactBranch <*> child selectedBranch)
