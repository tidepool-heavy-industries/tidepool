let observerCampaign = "inherited-response" :: CampaignLabel
let observerWave = "observer" :: ForkGroupLabel
let observerLabel = [label|observer|]
observer <- unfoldDeferred (batch observerCampaign observerWave) (child (coding @(Response (Text, Int -> Int)) currentCheckout (assignment observerLabel ("observe" :: Text))))
