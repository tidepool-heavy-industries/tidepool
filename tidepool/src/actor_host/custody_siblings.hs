let campaign = "custody" :: CampaignLabel
let wave = "siblings" :: ForkGroupLabel
let first = "first" :: BranchLabel
let second = "second" :: BranchLabel
siblings <- unfold (batch campaign wave) ((,) <$> child (coding @Text first projectHead ("first" :: Text)) <*> child (coding @Text second projectHead ("second" :: Text)))
