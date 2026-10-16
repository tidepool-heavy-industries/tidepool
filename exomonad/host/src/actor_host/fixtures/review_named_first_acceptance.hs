accepted <- pollResponse firstReview
display (case accepted of { ResponseReady receipt -> case responseValue receipt of { Produced (Accepted reviewed) -> reviewedBasis reviewed == reviewBasis firstInput && reviewedCandidate reviewed == reviewInput firstInput && reviewNotes reviewed == ["native inspection"] && reviewRationale reviewed == "native exact acceptance"; _ -> False }; _ -> False })
