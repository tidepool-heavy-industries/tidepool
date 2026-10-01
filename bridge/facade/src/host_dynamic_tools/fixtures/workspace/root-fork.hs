let campaign = "workspace-acceptance" :: CampaignLabel
let group = "root" :: ForkGroupLabel
let childLabel = [label|child|]
childWork <- unfoldDeferred (batch campaign group) (child @Text (withLifetime ActorOwned (coding projectHead (assignment childLabel ("fixture-child" :: Text)))))
