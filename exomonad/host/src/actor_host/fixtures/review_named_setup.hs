let firstInput = ReviewRequest (ExactScope baseOid ["review-fixture.txt"] "exact native review") (Candidate firstOid ["first check"] ["first gate"]) OwnerRepairs
Right (firstReview, _) <- requestReview "native exact review" firstInput
display (Reply.requestIdNumber (requestId firstReview))
