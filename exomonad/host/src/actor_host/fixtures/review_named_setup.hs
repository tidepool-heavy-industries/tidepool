let firstInput = ReviewRequest (ExactScope baseOid ["review-fixture.txt"] "exact native review") (Candidate firstOid ["first check"] ["first gate"]) OwnerRepairs
Right (firstReview, _) <- requestReview currentModel "native exact review" firstInput
display (Reply.requestIdNumber (requestId firstReview))
