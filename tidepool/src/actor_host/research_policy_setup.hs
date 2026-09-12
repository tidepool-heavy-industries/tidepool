let campaign = "research-policy" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let leafLabel = "researcher" :: BranchLabel
worker <- unfold (batch campaign group) (child (researching @Text leafLabel projectHead ()))
