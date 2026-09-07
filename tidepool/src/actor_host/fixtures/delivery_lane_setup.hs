let Right campaign = campaignLabel "delivery-package"
let Right leadWave = forkGroupLabel "lead"
let Right leadLabel = branchLabel "projection-lead"
let Right plannedLane = componentLane campaign RelationProjection baseline
before <- snapshot
lead <- unfold (batch campaign leadWave) (child (componentLead leadLabel baseline plannedLane))
