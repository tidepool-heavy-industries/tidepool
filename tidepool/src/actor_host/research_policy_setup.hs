let campaign = "research-policy" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let leafLabel = "researcher" :: Label
worker <- unfold (batch campaign group) (child (researching @Text projectHead (assignment leafLabel ())))
