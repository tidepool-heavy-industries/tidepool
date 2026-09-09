let Right campaign = campaignLabel "final-handoffs"
let Right wave = forkGroupLabel "lanes"
let Right leftLabel = branchLabel "left"
let Right rightLabel = branchLabel "right"
(left, leftProgress) <- unfold (batch campaign wave) (childWithProgress @Text @Text (coding leftLabel projectHead ("left" :: Text)))
