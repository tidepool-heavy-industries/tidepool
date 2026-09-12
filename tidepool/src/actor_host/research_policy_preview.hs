let campaign = "research-preview" :: CampaignLabel
let group = "coordinator" :: ForkGroupLabel
let leafLabel = "researcher" :: BranchLabel
let proposal = withForkBudget (ForkBudget 2 2) (researching @Text leafLabel projectHead ())
defaultPreview <- previewBranch (researching @Text leafLabel projectHead ())
requestedPreview <- previewBranch proposal
leafPreview <- previewBranch (researchingLeaf @Text leafLabel projectHead ())
zeroPreview <- previewBranch (withForkBudget (ForkBudget 0 2) (researching @Text leafLabel projectHead ()))
unboundedPreview <- previewBranch (coding @Text leafLabel projectHead ())
context <- actorContext
case (defaultPreview, requestedPreview, leafPreview, zeroPreview, unboundedPreview) of { (Right a, Right b, Right c, Right d, Right e) -> contextMaximumActiveChildren context == Nothing && allowanceWidth (previewEffectiveBudget e) == Nothing && previewDelegation e == CanFork && previewEffectiveBudget a == ForkAllowance 1 (Just 4) && previewEffectiveBudget b == ForkAllowance 2 (Just 2) && previewRequestedBudget b == Just (ForkBudget 2 2) && previewDelegation c == ForksOmitted && previewDelegation d == BudgetExhausted; _ -> False }
