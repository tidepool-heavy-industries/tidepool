let campaign = "cell-display" :: CampaignLabel
let wave = "children" :: ForkGroupLabel
let childLabel = "child" :: Label
worker <- unfold (batch campaign wave) (child (coding @Text projectHead (assignment childLabel ("inspect inherited page" :: Text))))
