do
  current <- C.getContext
  if any (ContextText.isInfixOf "context-own-call-sentinel") (toListOf C.visibleTexts current)
    then error "getContext included its own call" >> pure ()
    else pure ()
  C.putContext (over C.contextBlocks (<> [C.Text Nothing C.User "parent-curated" []]) current)
  C.setNextModel "parent-curated-model"
  let Right firstLabel = labelFromText "first"
  let Right secondLabel = labelFromText "second"
  let first = withLifetime ActorOwned (withModel (Literal "child-preparation-model") (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment firstLabel ("specialize this child" :: Text))))
  let second = withLifetime ActorOwned (withModel (Literal "child-preparation-model") (narrowed @'[Replies] @Text knownEffects (codingPolicy projectHead) (assignment secondLabel ("specialize this child" :: Text))))
  workers <- unfoldDeferred (batch ("context-acceptance" :: CampaignLabel) ("committed" :: ForkGroupLabel)) ((,) <$> child first <*> child second)
  pure True
-- Deferred children inherit the final scope, including bindings made after admission.
let curatedHelper value = retainedHelper value + 2 :: Int
retainedValue <- pure (40 :: Int)
