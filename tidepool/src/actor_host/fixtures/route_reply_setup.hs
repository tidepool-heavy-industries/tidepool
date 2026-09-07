let task = Task "plans/current/feature.md" "Deliver the feature" "Retain the exact evidence"
let Right campaign = campaignLabel "route-reply"
let Right leadWave = forkGroupLabel "lead"
let Right leadLabel = branchLabel "delivery-lead"
lead <- implement (batch campaign leadWave) leadLabel projectHead task
