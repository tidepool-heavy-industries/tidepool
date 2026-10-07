let AssignedTask assignedTask = reviewBasis sessionInput
let designLabel = "boundary-question" :: Text
let designWatch = "design-answer" :: Text
let slot = DesignSlot "plans/current/architecture.md" designLabel designWatch "planner" Medium
let ResponseReady repairedResult = state
let Produced repairedCandidate = responseValue repairedResult
let question = (designQuestion assignedTask repairedCandidate "Does preparation preserve the boundary?") { questionAlternatives = ["retain the gate", "expand acceptance"], questionUnblocks = ["feature review"] }
Right (expert, designReady) <- consultDesign slot question
