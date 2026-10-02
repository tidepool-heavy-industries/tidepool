do
  _ <- C.modifyContext (over C.editableTexts (ContextText.replace "parent-original" "must-not-publish"))
  C.setNextModel "must-not-publish-model"
  let deferred = withLifetime ActorOwned (coding @Text projectHead (assignment [label|must-not-launch|] ("cancelled transaction" :: Text)))
  worker <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("cancelled" :: ForkGroupLabel)) (child deferred)
  sleep (seconds 30)
  pure True
