let campaign = "workspace-descendants" :: CampaignLabel
let group = "child" :: ForkGroupLabel
let label = "grandchild" :: BranchLabel
grandchildWork <- unfold (batch campaign group) (child @Text (coding label boundHead ("fixture-grandchild" :: Text)))
