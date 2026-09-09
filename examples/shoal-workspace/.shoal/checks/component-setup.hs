let Right campaign = campaignLabel "delivery-package"
let Right leadLabel = branchLabel "projection-lead"
let Right task = component campaign RelationProjection baseline
before <- snapshot
leadWork <- unfold (taskGroup task) (childWithProgress @WorkProgress @Delivery (withLifetime SwarmOwned (withContext (selected taskContext) (componentLeadFrom leadLabel projectHead task))))
let (lead, leadQuestions) = leadWork
