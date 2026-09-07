let Right designCampaign = campaignLabel "declared-design"
let Right designWave = forkGroupLabel "architecture"
let Right designLabel = branchLabel "boundary-question"
let Right designWatch = watchLabel "design-answer"
let slot = DesignSlot "plans/current/architecture.md" (batch designCampaign designWave) designLabel designWatch "gpt-6-astra" Medium
let WatchReady repairedResult = state
let Right (Produced repairedCandidate) = settledValue repairedResult
let question = DesignQuestion (planPath (reviewAssignment sessionInput)) (candidateCommit repairedCandidate) "Does preparation preserve the boundary?" (checkedCommands repairedCandidate) ["retain the gate", "expand acceptance"] ["feature review"]
(expert, designReady) <- consultDesign slot question
