initialState <- pollResponse (forkedResponse worker)
let ResponseReady initialAnswer = initialState
let Produced initialCandidate = responseValue initialAnswer
(reviewer, reviewQuestions) <- reviewCandidate task (RetainedImplementer (forkedActor worker)) initialCandidate
