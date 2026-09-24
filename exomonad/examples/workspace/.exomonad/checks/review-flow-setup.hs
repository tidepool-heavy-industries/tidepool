{-# LANGUAGE QuasiQuotes #-}
let campaign = "review-continuation" :: CampaignLabel
let wave = "component" :: ForkGroupLabel
let workerLabel = [label|implementation|]
let reviewLabel = [label|review-produced-candidate|]
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Implement the feature" "Review the settled candidate without a model relay" ["feature.txt"] "read exact feature" []
(reviewer, initialReviewProgress) <- reviewCandidate task OwnerRepairs (Candidate sourceHead [] ["implementation pending"])
