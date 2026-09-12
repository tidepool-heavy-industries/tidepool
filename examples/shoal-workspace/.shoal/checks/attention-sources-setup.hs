let campaign = "attention-sources" :: CampaignLabel
let wave = "owners" :: ForkGroupLabel
let leftLabel = "left" :: BranchLabel
let rightLabel = "right" :: BranchLabel
(left, leftProgress) <- unfold (batch campaign wave) (childWithProgress @WorkProgress @Text (coding leftLabel projectHead ("left" :: Text)))
