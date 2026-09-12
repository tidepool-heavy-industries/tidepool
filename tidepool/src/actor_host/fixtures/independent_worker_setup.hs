let peersCampaign = "independent" :: CampaignLabel
let peersWave = "workers" :: ForkGroupLabel
let firstPeerLabel = "worker" :: Label
peer <- unfold (batch peersCampaign peersWave) (child (withLifetime SwarmOwned (withModel (Literal "gpt-5.6-sol") (withContext (selected id) (coding @Text projectHead (assignment firstPeerLabel ("First independent worker" :: Text)))))))
