let designCampaign = "declared-design" :: CampaignLabel
let designWave = "architecture" :: ForkGroupLabel
let designLabel = "boundary-question" :: BranchLabel
let designWatch = "design-answer" :: WatchLabel
let slot = DesignSlot "plans/current/architecture.md" (batch designCampaign designWave) designLabel designWatch "gpt-6-astra" Medium
let WatchReady repairedResult = state
let Right (Produced repairedCandidate) = settledValue repairedResult
let question = (designQuestion (reviewAssignment sessionInput) repairedCandidate "Does preparation preserve the boundary?") { questionAlternatives = ["retain the gate", "expand acceptance"], questionUnblocks = ["feature review"] }
(expert, designReady) <- consultDesign slot question
