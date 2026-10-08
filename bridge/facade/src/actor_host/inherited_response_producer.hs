let campaign = "inherited-producer" :: CampaignLabel
let wave = "worker" :: ForkGroupLabel
let workerLabel = [label|worker|]
worker <- unfoldDeferred (batch campaign wave) (child (withLifetime ActorOwned (narrowed @'[Replies] @(Text, Int -> Int) knownEffects (codingPolicy currentCheckout) (assignment workerLabel ("custody" :: Text)))))
