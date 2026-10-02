do
  let Right deferredLabel = labelFromText "async-must-not-launch"
  let deferred = withLifetime ActorOwned (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment deferredLabel ("rejected async cell" :: Text)))
  worker <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("async-failed" :: ForkGroupLabel)) (child deferred)
  error "intentional async deferred failure" >> pure True
