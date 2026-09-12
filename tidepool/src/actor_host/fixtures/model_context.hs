let parentOnly = 41 :: Int
let campaign = "model-context" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let exactLabel = "exact" :: Label
let selectedLabel = "selected" :: Label
workers <- unfold (batch campaign wave) ((,) <$> child (withModel (Literal "gpt-5.6-sol") (coding @Text projectHead (assignment exactLabel ("exact" :: Text)))) <*> child (withContext (selected (\task -> "Focused packet: " <> task)) (withModel (Literal "gpt-5.6-sol") (withEffort Medium (coding @Text projectHead (assignment selectedLabel ("selected" :: Text))))))
