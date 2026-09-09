let Right campaign = campaignLabel "attention-sources"
let Right wave = forkGroupLabel "owners"
let Right leftLabel = branchLabel "left"
let Right rightLabel = branchLabel "right"
(left, leftProgress) <- unfold (batch campaign wave) (childWithProgress @Attention @Text (coding leftLabel projectHead ("left" :: Text)))
