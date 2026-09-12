let peersCampaign = "independent" :: CampaignLabel
let peersWave = "workers" :: ForkGroupLabel
let firstPeerLabel = "worker" :: BranchLabel
peer <- unfold (batch peersCampaign peersWave) (child (withLifetime SwarmOwned (withModel "gpt-5.6-sol" (withContext (selected id) (coding @Text firstPeerLabel projectHead ("First independent worker" :: Text))))))
