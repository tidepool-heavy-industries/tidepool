let Right campaign = campaignLabel routeCampaign
let leadWave = "lead" :: ForkGroupLabel
let leadLabel = "delivery-lead" :: BranchLabel
let task = Task (batch campaign leadWave) "plans/current/feature.md" sourceHead "Deliver the feature" "Retain request ownership through automatic forwarding." ["feature.txt"] "Retain the exact evidence" []
lead <- unfold (taskGroup task) (child @Candidate (solTaskFrom leadLabel projectHead task))
