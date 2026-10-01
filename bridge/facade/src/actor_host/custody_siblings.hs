let campaign = "custody" :: CampaignLabel
let wave = "siblings" :: ForkGroupLabel
let first = [label|first|]
let second = [label|second|]
siblings <- unfoldDeferred (batch campaign wave) ((,) <$> child (withLifetime ActorOwned (coding @Text projectHead (assignment first ("first" :: Text)))) <*> child (withLifetime ActorOwned (coding @Text projectHead (assignment second ("second" :: Text)))))
