let effectfulObserverWave = "observer" :: ForkGroupLabel
let effectfulObserverLabel = [label|observer|]
observer <- unfoldDeferred (batch effectfulCampaign effectfulObserverWave) (child (withLifetime ActorOwned (coding @Text currentCheckout (assignment effectfulObserverLabel ("observer" :: Text)))))
