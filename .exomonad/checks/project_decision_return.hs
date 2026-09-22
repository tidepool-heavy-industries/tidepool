attention <- pollProgress reviewQuestions
let ProgressUpdate cursor openQuestions = attention
let answered = head (filter ((== "semantics") . questionKey) openQuestions)
let acceptedDecision = AcceptedDecision answered incorporatedHead "Preparation retains the boundary; visible UI acceptance remains separate." ["read exact plan at resulting head"]
Right clarification <- updateRequest (forkedResponse reviewer) (decisionContext acceptedDecision)
pollResponse (forkedResponse reviewer)
