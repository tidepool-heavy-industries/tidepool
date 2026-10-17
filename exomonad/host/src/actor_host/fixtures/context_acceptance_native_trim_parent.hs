import qualified Tidepool.Agent.Contract as A
let curatedHelper value = retainedHelper value + 2 :: Int
retainedValue <- pure (40 :: Int)

do
  current <- C.getContext
  let trimResult body = if ContextText.isInfixOf "native-trim-result-tail" body
        then C.trimText "repetitive build output" "Build succeeded."
        else body
  if any (ContextText.isInfixOf "native-trim-own-call") (toListOf C.visibleTexts current)
    then error "getContext included its own call" >> pure ()
    else pure ()
  C.ContextStaged _ <- C.stageContext (C.contextPatch
    [C.replaceText target updated | target <- C.textTargets current,
      let updated = trimResult (C.targetText target), updated /= C.targetText target])
  C.setNextModel (C.CustomModel C.OpenAIPlan "test-model")
  C.setNextReasoning (C.OpenAIReasoning C.OpenAIHigh)
  Right captured <- checkpoint "curated parent snapshot"
  idle <- mapM (\name -> spawnSubagent (ForkCtx captured) SameDir
    ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
      { spawnModel = Just (ProviderModel "test-model"), spawnLabel = Just name }))
    ["first", "second"]
  workers <- mapM (\admitted -> case admitted of
    Left issue -> error (ContextText.pack (show issue))
    Right actor -> do
      Right reply <- request @Text actor ("reuse the curated build result" :: Text) defaultRequestOptions
      pure reply) idle
  pure True
