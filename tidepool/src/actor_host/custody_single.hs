let campaign = "custody-single" :: CampaignLabel
let wave = "worker" :: ForkGroupLabel
let label = "worker" :: BranchLabel
worker <- unfold (batch campaign wave) (child (coding @Text label projectHead ("custody" :: Text)))
