let peersCampaign = "independent" :: CampaignLabel
let peersWave = "workers" :: ForkGroupLabel
let firstPeerLabel = [label|worker|]
peer <- unfold (batch peersCampaign peersWave) (child (withLifetime SwarmOwned (withModel (Literal "gpt-6-sol") (withContext (selected id) (coding @Text projectHead (assignment firstPeerLabel ("First independent worker" :: Text)))))))
