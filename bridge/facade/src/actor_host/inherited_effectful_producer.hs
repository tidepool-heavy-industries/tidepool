let effectfulCampaign = "effectful-closure" :: CampaignLabel
let effectfulWave = "producer" :: ForkGroupLabel
let effectfulProducerLabel = [label|producer|]
worker <- unfold (batch effectfulCampaign effectfulWave) (child (coding @(() -> Eff CodingEffects Cmd.Job) currentCheckout (assignment effectfulProducerLabel ())))
