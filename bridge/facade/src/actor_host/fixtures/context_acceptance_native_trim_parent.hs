do
  current <- C.getContext
  let trimResult body = if ContextText.isInfixOf "native-trim-result-tail" body
        then C.trimText "repetitive build output" "Build succeeded."
        else body
  if any (ContextText.isInfixOf "native-trim-own-call") (toListOf C.visibleTexts current)
    then error "getContext included its own call" >> pure ()
    else pure ()
  C.putContext (over C.editableTexts trimResult current)
  C.setNextModel "test-model"
  C.setNextEffort C.High
  let Right firstLabel = labelFromText "first"
  let Right secondLabel = labelFromText "second"
  let first = withLifetime ActorOwned (withModel (Literal "test-model") (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment firstLabel ("reuse the curated build result" :: Text))))
  let second = withLifetime ActorOwned (withModel (Literal "test-model") (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment secondLabel ("reuse the curated build result" :: Text))))
  workers <- unfoldDeferred (batch ("native-trim-acceptance" :: CampaignLabel) ("committed" :: ForkGroupLabel)) ((,) <$> child first <*> child second)
  pure True
let curatedHelper value = retainedHelper value + 2 :: Int
retainedValue <- pure (40 :: Int)
