accepted <- pollResponse assignedReview
let acceptedExactly = case accepted of { ResponseReady receipt -> case responseValue receipt of { Produced (Accepted reviewed) -> reviewedBasis reviewed == AssignedTask assigned && reviewedCandidate reviewed == reviewInput assignedInput && reviewNotes reviewed == ["native revised inspection"] && reviewRationale reviewed == "native assigned acceptance"; _ -> False }; _ -> False }
stopped <- stopAgent (responseActor assignedReview)
display (acceptedExactly && case stopped of { StoppedNow -> True; AlreadyStopped -> True; _ -> False })
