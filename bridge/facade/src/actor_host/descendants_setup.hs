let campaign = "descendants" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let childLabel = [label|descendants-child|]
childResponse <- unfold (batch campaign group) (child (researching @Int projectHead (assignment childLabel (1 :: Int))))
