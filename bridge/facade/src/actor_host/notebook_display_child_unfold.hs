let campaign = "cell-display" :: CampaignLabel
let wave = "children" :: ForkGroupLabel
let childLabel = [label|child|]
worker <- unfoldDeferred (batch campaign wave) (child (withLifetime ActorOwned (coding @Text projectHead (assignment childLabel ("inspect inherited page" :: Text)))))
