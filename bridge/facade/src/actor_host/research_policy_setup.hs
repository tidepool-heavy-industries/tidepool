let campaign = "research-policy" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let leafLabel = [label|researcher|]
worker <- unfold (batch campaign group) (child (researching @Text projectHead (assignment leafLabel ())))
