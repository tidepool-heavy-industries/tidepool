let campaign = "custody-single" :: CampaignLabel
let wave = "worker" :: ForkGroupLabel
let label = "worker" :: Label
worker <- unfold (batch campaign wave) (child (coding @Text projectHead (assignment label ("custody" :: Text))))
