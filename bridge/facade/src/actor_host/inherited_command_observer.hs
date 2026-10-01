let campaign = "inherited-command" :: CampaignLabel
let wave = "observers" :: ForkGroupLabel
let childLabel = [label|observer|]
observer <- unfoldDeferred (batch campaign wave) (child (withLifetime ActorOwned (coding @Text currentCheckout (assignment childLabel ("inspect inherited job" :: Text)))))
