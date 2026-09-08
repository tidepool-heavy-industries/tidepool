let Right campaign = campaignLabel "attention-sources"
let Right wave = forkGroupLabel "owners"
let Right leftLabel = branchLabel "left"
let Right rightLabel = branchLabel "right"
((left, leftProgress), (right, rightProgress)) <- unfold (batch campaign wave) ((,) <$> childWithProgress @Attention @Text (coding leftLabel projectHead ("left" :: Text)) <*> childWithProgress @Attention @Text (coding rightLabel projectHead ("right" :: Text)))
let Right consumerLabel = branchLabel "status-consumer"
consumer <- unfold (batch campaign wave) (child (coding @Text consumerLabel projectHead ([] :: [AttentionSource])))
