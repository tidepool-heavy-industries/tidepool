let campaign = "custody" :: CampaignLabel
let wave = "siblings" :: ForkGroupLabel
let first = [label|first|]
let second = [label|second|]
siblings <- unfold (batch campaign wave) ((,) <$> child (coding @Text projectHead (assignment first ("first" :: Text))) <*> child (coding @Text projectHead (assignment second ("second" :: Text))))
