let campaign = "custody-single" :: CampaignLabel
let wave = "worker" :: ForkGroupLabel
let workerLabel = [label|worker|]
worker <- unfoldDeferred (batch campaign wave) (child (coding @Text projectHead (assignment workerLabel ("custody" :: Text))))
