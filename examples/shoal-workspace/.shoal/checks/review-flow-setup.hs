let campaign = "review-continuation" :: CampaignLabel
let wave = "component" :: ForkGroupLabel
let workerLabel = "implementation" :: BranchLabel
let reviewLabel = "review-produced-candidate" :: RequestLabel
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Implement the feature" "Review the settled candidate without a model relay" ["feature.txt"] "read exact feature" []
(reviewer, initialReviewProgress) <- reviewCandidate task OwnerRepairs (Candidate sourceHead [] ["implementation pending"])
