let Right campaign = campaignLabel "review-continuation"
let Right wave = forkGroupLabel "component"
let Right workerLabel = branchLabel "implementation"
let Right reviewLabel = requestLabel "review-produced-candidate"
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Implement the feature" "Review the settled candidate without a model relay" ["feature.txt"] "read exact feature" []
(reviewer, initialReviewProgress) <- reviewCandidate task OwnerRepairs (Candidate sourceHead [] ["implementation pending"])
