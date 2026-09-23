let campaign = "workspace-descendants" :: CampaignLabel
let group = "child" :: ForkGroupLabel
let grandchildLabel = [label|grandchild|]
grandchildWork <- unfold (batch campaign group) (child @Text (coding currentCheckout (assignment grandchildLabel ("fixture-grandchild" :: Text))))
