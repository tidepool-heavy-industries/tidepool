let Right peerObserverWave = forkGroupLabel "observer"
let Right secondPeerLabel = branchLabel "observer"
peerObserver <- unfold (batch peersCampaign peerObserverWave) (child (withLifetime SwarmOwned (withModel "gpt-5.6-sol" (withContext (selected (const "Retain the exact peer handle for a followup")) (coding @Text secondPeerLabel projectHead (forkedActor peer))))))
