let Right campaignLabelValue = campaignLabel "progress-routes"
let Right wave = forkGroupLabel "workers"
let Right producerLabel = branchLabel "producer"
(producer, updates) <- unfold (batch campaignLabelValue wave) (childWithProgress @WorkProgress @Text (coding producerLabel projectHead ("inspect contract" :: Text)))
