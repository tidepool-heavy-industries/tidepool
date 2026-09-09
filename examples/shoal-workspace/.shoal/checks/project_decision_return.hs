attention <- pollProgress reviewQuestions
let ProgressUpdate _ WorkProgress { workQuestions = openQuestions } = attention
let answered = head (filter ((== "semantics") . questionKey) openQuestions)
let acceptedDecision = AcceptedDecision answered incorporatedHead "Preparation retains the boundary; visible UI acceptance remains separate." ["read exact plan at resulting head"]
Right clarification <- updateDecision (forkedResponse reviewer) acceptedDecision
pollResponse (forkedResponse reviewer)
