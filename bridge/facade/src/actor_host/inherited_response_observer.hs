let observerCampaign = "inherited-response" :: CampaignLabel
let observerWave = "observer" :: ForkGroupLabel
let observerLabel = [label|observer|]
observer <- unfoldDeferred (batch observerCampaign observerWave) (child (withLifetime ActorOwned (narrowed @'[Replies, Watches] @(Response (Text, Int -> Int)) knownEffects (codingPolicy currentCheckout) (assignment observerLabel ("observe" :: Text)))))
