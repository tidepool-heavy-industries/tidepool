let Right campaign = campaignLabel "delivery-package"
let Right leadLabel = branchLabel "projection-lead"
let Right task = component campaign RelationProjection baseline
before <- snapshot
leadWork <- unfold (taskGroup task) (childWithProgress @Attention @Delivery (withLifetime SwarmOwned (componentLead leadLabel task)))
let (lead, leadQuestions) = leadWork
