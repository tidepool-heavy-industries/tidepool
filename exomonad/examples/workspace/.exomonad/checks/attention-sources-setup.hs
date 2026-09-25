{-# LANGUAGE QuasiQuotes #-}
let campaign = "attention-sources" :: CampaignLabel
let wave = "owners" :: ForkGroupLabel
let leftLabel = [label|left|]
let rightLabel = [label|right|] :: Label
(left, leftProgress) <- unfold (batch campaign wave) (childWithProgress @WorkProgress @Text (coding projectHead (assignment leftLabel ("left" :: Text))))
