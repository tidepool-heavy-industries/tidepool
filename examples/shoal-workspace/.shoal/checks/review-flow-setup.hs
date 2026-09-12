let campaign = "review-continuation" :: CampaignLabel
let wave = "component" :: ForkGroupLabel
let workerLabel = "implementation" :: Label
let reviewLabel = "review-produced-candidate" :: Label
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Implement the feature" "Review the settled candidate without a model relay" ["feature.txt"] "read exact feature" []
(reviewer, initialReviewProgress) <- reviewCandidate task OwnerRepairs (Candidate sourceHead [] ["implementation pending"])
