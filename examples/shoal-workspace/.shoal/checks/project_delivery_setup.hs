let campaign = "project-delivery" :: CampaignLabel
let workGroup = "feature" :: ForkGroupLabel
let task = Task (batch campaign workGroup) "plans/current/feature.md" sourceHead "Implement the feature" "Preserve the product boundary during preparation." ["feature.txt"] "Preserve the product gate" []
let workerLabel = "implement" :: Label
(worker, workerQuestions) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) (withContext (selected taskContext) (solTaskFrom workerLabel projectHead task)))
