let campaign = "inherited-producer" :: CampaignLabel
let wave = "worker" :: ForkGroupLabel
let workerLabel = [label|worker|]
worker <- unfoldDeferred (batch campaign wave) (child (withLifetime ActorOwned (coding @(Text, Int -> Int) currentCheckout (assignment workerLabel ("custody" :: Text)))))
