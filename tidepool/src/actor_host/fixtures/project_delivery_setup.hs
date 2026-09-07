let task = Task "plans/current/feature.md" "Implement the feature" "Preserve the product gate"
let Right campaign = campaignLabel "project-delivery"
let Right implementationWave = forkGroupLabel "implementation"
let Right implementationLabel = branchLabel "feature"
let Right reviewWave = forkGroupLabel "review"
let Right reviewLabel = branchLabel "feature-review"
worker <- implement (batch campaign implementationWave) implementationLabel projectHead task
reviewRoute <- route (awaitSettledFork worker) (\settled -> case settled of { ReplyUnavailable failure -> error (T.pack (show failure)); ReplyAvailable answer -> do { _ <- reviewCandidate (batch campaign reviewWave) reviewLabel task (forkedActor worker) (responseValue answer); pure () } })
