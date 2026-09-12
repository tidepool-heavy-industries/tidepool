let peerObserverWave = "observer" :: ForkGroupLabel
let secondPeerLabel = "observer" :: Label
peerObserver <- unfold (batch peersCampaign peerObserverWave) (child (withLifetime SwarmOwned (withModel (Literal "gpt-5.6-sol") (withContext (selected (const "Retain the exact peer handle for a followup")) (coding @Text projectHead (assignment secondPeerLabel (responseActor peer)))))))
