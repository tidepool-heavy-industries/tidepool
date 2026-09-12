let campaign = "workspace-descendants" :: CampaignLabel
let group = "child" :: ForkGroupLabel
let label = "grandchild" :: Label
grandchildWork <- unfold (batch campaign group) (child @Text (coding boundHead (assignment label ("fixture-grandchild" :: Text))))
