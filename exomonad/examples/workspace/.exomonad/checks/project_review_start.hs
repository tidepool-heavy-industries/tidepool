initialState <- pollResponse worker
let ResponseReady initialAnswer = initialState
let Produced initialCandidate = responseValue initialAnswer
Right (reviewer, reviewQuestions) <- reviewCandidate work (RetainedImplementer (responseActor worker)) initialCandidate
