let campaignLabelValue = "progress-routes" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let producerLabel = "producer" :: BranchLabel
(producer, updates) <- unfold (batch campaignLabelValue wave) (childWithProgress @WorkProgress @Text (coding producerLabel projectHead ("inspect contract" :: Text)))
