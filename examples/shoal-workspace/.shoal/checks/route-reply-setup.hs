let Right campaign = campaignLabel routeCampaign
let Right leadWave = forkGroupLabel "lead"
let Right leadLabel = branchLabel "delivery-lead"
let task = Task (batch campaign leadWave) "plans/current/feature.md" "HEAD" "Deliver the feature" "Retain request ownership through automatic forwarding." ["feature.txt"] "Retain the exact evidence" []
lead <- unfold (taskGroup task) (child @Candidate (solTask leadLabel task))
