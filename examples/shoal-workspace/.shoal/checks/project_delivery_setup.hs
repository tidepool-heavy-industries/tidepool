let Right campaign = campaignLabel "project-delivery"
let Right workGroup = forkGroupLabel "feature"
let task = Task (batch campaign workGroup) "plans/current/feature.md" sourceHead "Implement the feature" "Preserve the product boundary during preparation." ["feature.txt"] "Preserve the product gate" []
let Right workerLabel = branchLabel "implement"
(worker, workerQuestions) <- unfold (taskGroup task) (childWithProgress @Attention @(Outcome Candidate) (withContext (selected taskContext) (solTaskFrom workerLabel projectHead task)))
