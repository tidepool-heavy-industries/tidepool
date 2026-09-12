let campaign = "delivery-package" :: CampaignLabel
let leadLabel = "projection-lead" :: BranchLabel
let Right task = component campaign RelationProjection baseline
before <- snapshot
leadWork <- unfold (taskGroup task) (childWithProgress @WorkProgress @Delivery (withLifetime SwarmOwned (withContext (selected taskContext) (componentLeadFrom leadLabel projectHead task))))
let (lead, leadQuestions) = leadWork
