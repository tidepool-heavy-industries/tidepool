let peerObserverWave = "observer" :: ForkGroupLabel
let secondPeerLabel = "observer" :: BranchLabel
peerObserver <- unfold (batch peersCampaign peerObserverWave) (child (withLifetime SwarmOwned (withModel "gpt-5.6-sol" (withContext (selected (const "Retain the exact peer handle for a followup")) (coding @Text secondPeerLabel projectHead (forkedActor peer))))))
