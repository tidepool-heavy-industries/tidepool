let effectfulObserverWave = "observer" :: ForkGroupLabel
let effectfulObserverLabel = [label|observer|]
observer <- unfold (batch effectfulCampaign effectfulObserverWave) (child (coding @Text currentCheckout (assignment effectfulObserverLabel ("observer" :: Text))))
