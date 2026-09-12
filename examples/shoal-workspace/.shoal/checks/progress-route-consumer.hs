let consumerLabel = "consumer" :: BranchLabel
consumer <- unfold (batch campaignLabelValue wave) (child (coding @Text consumerLabel projectHead ([] :: Attention)))
