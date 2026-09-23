let group = "agentref-check" :: CampaignLabel
let wave = "admission" :: ForkGroupLabel
let worker = "worker" :: Label
response <- unfold (batch group wave) (child @Text (coding projectHead (assignment worker ("reply ping" :: Text))))
let Just receipt = responseAdmission response
sendMessage (admittedAgent receipt) "ping"
