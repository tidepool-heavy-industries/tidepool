let WatchReady incorporationResult = incorporation
let Right (Incorporated amendment incorporatedHead incorporationChecks) = settledValue incorporationResult
let acceptedDecision = AcceptedDecision semantics incorporatedHead "Preparation retains the boundary; visible UI acceptance remains separate." incorporationChecks
let assignment = withDecision acceptedDecision (reviewAssignment sessionInput)
let remainingQuestions = resolveQuestion acceptedDecision openQuestions
reportProgress (WorkProgress [] remainingQuestions)
let newer = semantics { questionDetails = (questionDetails semantics) { questionFinding = "A new owning consumer contradicts the earlier answer." } }
let changedQuestions = raiseQuestion newer openQuestions
inspectFull (map questionKey remainingQuestions == ["product-gate"], resolveQuestion acceptedDecision changedQuestions == changedQuestions, raiseQuestion semantics firstQuestions == firstQuestions, taskSource assignment == incorporatedHead)
let Right consumerLabel = branchLabel "implement"
(consumer, consumerQuestions) <- unfold (taskGroup assignment) (childWithProgress @WorkProgress @(Outcome Candidate) (withContext (selected taskContext) (solTask consumerLabel assignment)))
pollReply sessionReply
