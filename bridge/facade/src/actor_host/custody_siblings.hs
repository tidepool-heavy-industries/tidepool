let campaign = "custody" :: CampaignLabel
let wave = "siblings" :: ForkGroupLabel
let first = "first" :: Label
let second = "second" :: Label
siblings <- unfold (batch campaign wave) ((,) <$> child (coding @Text projectHead (assignment first ("first" :: Text))) <*> child (coding @Text projectHead (assignment second ("second" :: Text))))
