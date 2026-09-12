let campaign = "workspace-acceptance" :: CampaignLabel
let group = "root" :: ForkGroupLabel
let label = "child" :: BranchLabel
childWork <- unfold (batch campaign group) (child @Text (coding label projectHead ("fixture-child" :: Text)))
