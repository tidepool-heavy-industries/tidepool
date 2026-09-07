let Right campaign = campaignLabel "custody"
let Right wave = forkGroupLabel "siblings"
let Right first = branchLabel "first"
let Right second = branchLabel "second"
siblings <- unfold (batch campaign wave) ((,) <$> child (coding @Text first projectHead ("first" :: Text)) <*> child (coding @Text second projectHead ("second" :: Text)))
