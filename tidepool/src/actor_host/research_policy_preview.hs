let campaign = case campaignLabel "research-preview" of { Right value -> value; Left _ -> error "fixture label" }
let group = case forkGroupLabel "coordinator" of { Right value -> value; Left _ -> error "fixture label" }
let leafLabel = case branchLabel "researcher" of { Right value -> value; Left _ -> error "fixture label" }
let proposal = withForkBudget (ForkBudget 2 2) (researching @Text leafLabel projectHead ())
defaultPreview <- previewBranch (researching @Text leafLabel projectHead ())
requestedPreview <- previewBranch proposal
leafPreview <- previewBranch (researchingLeaf @Text leafLabel projectHead ())
zeroPreview <- previewBranch (withForkBudget (ForkBudget 0 2) (researching @Text leafLabel projectHead ()))
case (defaultPreview, requestedPreview, leafPreview, zeroPreview) of { (Right a, Right b, Right c, Right d) -> previewEffectiveBudget a == ForkBudget 1 4 && previewEffectiveBudget b == ForkBudget 2 2 && previewRequestedBudget b == Just (ForkBudget 2 2) && previewDelegation c == ForksOmitted && previewDelegation d == BudgetExhausted; _ -> False }
