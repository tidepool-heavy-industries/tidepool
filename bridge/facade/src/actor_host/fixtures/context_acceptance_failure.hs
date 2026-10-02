do
  _ <- C.modifyContext (over C.editableTexts (ContextText.replace "parent-original" "must-not-publish"))
  C.setNextModel "must-not-publish-model"
  let deferred = withLifetime ActorOwned (coding @Text projectHead (assignment [label|must-not-launch|] ("rejected transaction" :: Text)))
  worker <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("failed" :: ForkGroupLabel)) (child deferred)
  error "intentional context transaction failure" >> pure True
