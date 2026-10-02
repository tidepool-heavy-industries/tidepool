do
  current <- C.getContext
  if any (ContextText.isInfixOf "context-own-call-sentinel") (toListOf C.visibleTexts current)
    then error "getContext included its own call" >> pure ()
    else pure ()
  C.putContext (over C.editableTexts (ContextText.replace "parent-original" "parent-curated") current)
  C.setNextModel "parent-curated-model"
  let first = withLifetime ActorOwned (withModel (Literal "test-model") (coding @Text projectHead (assignment [label|first|] ("specialize this child" :: Text))))
  let second = withLifetime ActorOwned (withModel (Literal "test-model") (coding @Text projectHead (assignment [label|second|] ("specialize this child" :: Text))))
  workers <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("committed" :: ForkGroupLabel)) ((,) <$> child first <*> child second)
  pure True
