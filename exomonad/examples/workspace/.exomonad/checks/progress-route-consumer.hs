{-# LANGUAGE QuasiQuotes #-}
let consumerLabel = [label|consumer|]
consumer <- unfoldDeferred (batch campaignLabelValue wave) (child (withLifetime ActorOwned $ coding @Text projectHead (assignment consumerLabel ([] :: Attention))))
