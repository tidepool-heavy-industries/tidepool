let question = Question "boundary" (DesignQuestion "plan.md" baseOid "retain authority" ["source evidence"] [] [])
let decision = AcceptedDecision question baseOid "preserve the boundary" ["decision evidence"]
let assigned = (task "assigned-review" "review implementation" ["review-fixture.txt"] "assigned acceptance" baseOid) { planPath = "plan.md", rationale = "retained reason", acceptedDecisions = [decision] }
let assignedInput = ReviewRequest (AssignedTask assigned) (Candidate revisedOid ["candidate check"] ["open gate"]) OwnerRepairs
Right (assignedReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor firstReview) assignedInput defaultRequestOptions
display (Reply.requestIdNumber (requestId assignedReview))
