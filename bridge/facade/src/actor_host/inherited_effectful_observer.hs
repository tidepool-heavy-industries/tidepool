let effectfulObserverWave = "observer" :: ForkGroupLabel
let effectfulObserverLabel = [label|observer|]
observer <- unfoldDeferred (batch effectfulCampaign effectfulObserverWave) (child (coding @Text currentCheckout (assignment effectfulObserverLabel ("observer" :: Text))))
