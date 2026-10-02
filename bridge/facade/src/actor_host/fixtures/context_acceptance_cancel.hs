do
  _ <- C.modifyContext (over C.editableTexts (ContextText.replace "parent-original" "must-not-publish"))
  C.setNextModel "must-not-publish-model"
  let Right deferredLabel = labelFromText "must-not-launch"
  let deferred = withLifetime ActorOwned (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment deferredLabel ("cancelled transaction" :: Text)))
  worker <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("cancelled" :: ForkGroupLabel)) (child deferred)
  sleep (seconds 30)
  pure True
