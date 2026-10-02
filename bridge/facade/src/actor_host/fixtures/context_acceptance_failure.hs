do
  let trimResult body = if ContextText.isInfixOf "context-setup-retained-result" body
        then C.trimText "must not publish failed trim" "must-not-publish"
        else ContextText.replace "parent-original" "must-not-publish" body
  staged <- C.modifyContext (over C.editableTexts trimResult)
  if any (ContextText.isInfixOf "must not publish failed trim") (toListOf C.editableTexts staged)
    then pure ()
    else error "native result trim was not staged" >> pure ()
  C.setNextModel "must-not-publish-model"
  C.setNextEffort C.High
  let Right deferredLabel = labelFromText "must-not-launch"
  let deferred = withLifetime ActorOwned (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment deferredLabel ("rejected transaction" :: Text)))
  worker <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("failed" :: ForkGroupLabel)) (child deferred)
  error "intentional context transaction failure" >> pure True
