{-# LANGUAGE QuasiQuotes #-}
let consumerLabel = [label|consumer|]
consumer <- unfold (batch campaignLabelValue wave) (child (coding @Text projectHead (assignment consumerLabel ([] :: Attention))))
