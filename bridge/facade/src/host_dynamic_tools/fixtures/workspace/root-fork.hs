let campaign = "workspace-acceptance" :: CampaignLabel
let group = "root" :: ForkGroupLabel
let label = "child" :: Label
childWork <- unfold (batch campaign group) (child @Text (coding projectHead (assignment label ("fixture-child" :: Text))))
