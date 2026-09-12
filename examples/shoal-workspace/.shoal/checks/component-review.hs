(reviewer, questions) <- reviewCandidate sessionInput OwnerRepairs candidate
let readyLabel = "review-ready" :: WatchLabel
ready <- watch readyLabel (awaitSettled reviewer)
