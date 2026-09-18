(reviewer, questions) <- reviewCandidate sessionInput OwnerRepairs candidate
let Right readyLabel = watchLabel "review-ready"
ready <- watch readyLabel (awaitSettledFork reviewer)
