let campaign = "inherited-producer" :: CampaignLabel
let wave = "worker" :: ForkGroupLabel
let workerLabel = [label|worker|]
worker <- unfold (batch campaign wave) (child (coding @(Text, Int -> Int) currentCheckout (assignment workerLabel ("custody" :: Text))))
