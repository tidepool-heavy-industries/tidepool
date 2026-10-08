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
  C.putContext (over C.editableTexts trimResult current)
  C.setNextModel "test-model"
  C.setNextEffort C.High
  Right captured <- checkpoint "curated parent snapshot"
  idle <- mapM (\name -> spawnSubagent (ForkCtx captured) SameDir
    ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
      { spawnModel = Just (Literal "test-model"), spawnLabel = Just name }))
    ["first", "second"]
  workers <- mapM (\admitted -> case admitted of
    Left issue -> error (show issue)
    Right actor -> do
      Right reply <- request @Text actor ("reuse the curated build result" :: Text) defaultRequestOptions
      pure reply) idle
  pure True
