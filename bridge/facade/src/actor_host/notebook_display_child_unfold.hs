let campaign = "cell-display" :: CampaignLabel
let wave = "children" :: ForkGroupLabel
let childLabel = [label|child|]
worker <- unfold (batch campaign wave) (child (coding @Text projectHead (assignment childLabel ("inspect inherited page" :: Text))))
