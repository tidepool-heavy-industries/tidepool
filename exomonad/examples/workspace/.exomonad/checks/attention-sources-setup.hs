{-# LANGUAGE QuasiQuotes #-}
let campaign = "attention-sources" :: CampaignLabel
let wave = "owners" :: ForkGroupLabel
let leftLabel = [label|left|]
let rightLabel = [label|right|] :: Label
(left, leftProgress) <- unfoldDeferred (batch campaign wave) (childWithProgress @WorkProgress @Text (withLifetime ActorOwned $ coding projectHead (assignment leftLabel ("left" :: Text))))
