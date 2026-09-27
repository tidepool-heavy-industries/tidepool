attention <- pollProgress reviewQuestions
let ProgressUpdate _ progress = attention
let openQuestions = workQuestions progress
let answered = head (filter ((== "semantics") . questionKey) openQuestions)
let acceptedDecision = AcceptedDecision answered incorporatedHead "Preparation retains the boundary; visible UI acceptance remains separate." ["read exact plan at resulting head"]
Right clarification <- updateDecision reviewer acceptedDecision
pollResponse reviewer
