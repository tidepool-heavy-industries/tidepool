let campaign = "research-policy" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let leafLabel = [label|researcher|]
worker <- unfoldDeferred (batch campaign group) (child (withLifetime ActorOwned (researching @Text projectHead (assignment leafLabel ()))))
