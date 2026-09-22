let campaignLabelValue = "progress-routes" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let producerLabel = "producer" :: Label
(producer, updates) <- unfold (batch campaignLabelValue wave) (childWithProgress @WorkProgress @Text (coding projectHead (assignment producerLabel ("inspect contract" :: Text))))
