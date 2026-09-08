let Right designCampaign = campaignLabel "declared-design"
let Right designWave = forkGroupLabel "architecture"
let Right designLabel = branchLabel "boundary-question"
let Right designWatch = watchLabel "design-answer"
let slot = DesignSlot "plans/current/architecture.md" (batch designCampaign designWave) designLabel designWatch "gpt-6-astra" Medium
let WatchReady repairedResult = state
let Right (Produced repairedCandidate) = settledValue repairedResult
let question = (designQuestion (reviewAssignment sessionInput) repairedCandidate "Does preparation preserve the boundary?") { questionAlternatives = ["retain the gate", "expand acceptance"], questionUnblocks = ["feature review"] }
(expert, designReady) <- consultDesign slot question
