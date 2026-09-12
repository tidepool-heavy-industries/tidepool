let leadWave = "lead" :: ForkGroupLabel
let leadLabel = "delivery-lead" :: Label
let task = Task (batch routeCampaign leadWave) "plans/current/feature.md" sourceHead "Deliver the feature" "Retain request ownership through automatic forwarding." ["feature.txt"] "Retain the exact evidence" []
lead <- unfold (taskGroup task) (child @Candidate (solTaskFrom leadLabel projectHead task))
