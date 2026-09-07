let Right peersCampaign = campaignLabel "independent"
let Right peersWave = forkGroupLabel "workers"
let Right firstPeerLabel = branchLabel "worker"
peer <- unfold (batch peersCampaign peersWave) (child (withLifetime SwarmOwned (withModel "gpt-5.6-sol" (withContext (selected id) (coding @Text firstPeerLabel projectHead ("First independent worker" :: Text))))))
