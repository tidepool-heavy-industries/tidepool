do
  _ <- C.modifyContext (over C.editableTexts (ContextText.replace "parent-original" "must-not-publish"))
  C.setNextModel "must-not-publish-model"
  C.setNextEffort C.High
  let Right deferredLabel = labelFromText "must-not-launch"
  let deferred = withLifetime ActorOwned (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment deferredLabel ("rejected transaction" :: Text)))
  worker <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("failed" :: ForkGroupLabel)) (child deferred)
  error "intentional context transaction failure" >> pure True
