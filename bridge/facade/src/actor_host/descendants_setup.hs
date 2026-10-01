let campaign = "descendants" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let childLabel = [label|descendants-child|]
childResponse <- unfoldDeferred (batch campaign group) (child (withLifetime ActorOwned (researching @Int projectHead (assignment childLabel (1 :: Int)))))
