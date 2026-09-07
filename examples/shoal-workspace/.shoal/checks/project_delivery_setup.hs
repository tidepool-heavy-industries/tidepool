let Right campaign = campaignLabel "project-delivery"
let Right workGroup = forkGroupLabel "feature"
let task = Task (batch campaign workGroup) "plans/current/feature.md" sourceHead "Implement the feature" "Preserve the product boundary during preparation." ["feature.txt"] "Preserve the product gate" []
(worker, workerQuestions) <- implement task
