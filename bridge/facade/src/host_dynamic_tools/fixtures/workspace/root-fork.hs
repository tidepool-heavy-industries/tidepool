let campaign = "workspace-acceptance" :: CampaignLabel
let group = "root" :: ForkGroupLabel
let childLabel = [label|child|]
childWork <- unfold (batch campaign group) (child @Text (coding projectHead (assignment childLabel ("fixture-child" :: Text))))
