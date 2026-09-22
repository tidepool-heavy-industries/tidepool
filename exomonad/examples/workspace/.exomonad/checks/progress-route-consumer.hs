let consumerLabel = "consumer" :: Label
consumer <- unfold (batch campaignLabelValue wave) (child (coding @Text projectHead (assignment consumerLabel ([] :: Attention))))
