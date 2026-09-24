{-# LANGUAGE QuasiQuotes #-}
let leadWave = "lead" :: ForkGroupLabel
let leadLabel = [label|delivery-lead|]
let task = Task (batch routeCampaign leadWave) "plans/current/feature.md" sourceHead "Deliver the feature" "Retain request ownership through automatic forwarding." ["feature.txt"] "Retain the exact evidence" []
lead <- unfold (taskGroup task) (child @Candidate (solTaskFrom leadLabel Medium projectHead task))
