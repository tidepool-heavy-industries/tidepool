{-# LANGUAGE QuasiQuotes #-}
let campaignLabelValue = "progress-routes" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let producerLabel = [label|producer|]
(producer, updates) <- unfoldDeferred (batch campaignLabelValue wave) (childWithProgress @WorkProgress @Text (withLifetime ActorOwned $ coding projectHead (assignment producerLabel ("inspect contract" :: Text))))
