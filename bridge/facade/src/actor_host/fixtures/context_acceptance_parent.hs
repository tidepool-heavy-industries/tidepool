import qualified Tidepool.Agent.Contract as A
let curatedHelper value = retainedHelper value + 2 :: Int
retainedValue <- pure (40 :: Int)

do
  current <- C.getContext
  if any (ContextText.isInfixOf "context-own-call-sentinel") (toListOf C.visibleTexts current)
    then error "getContext included its own call" >> pure ()
    else pure ()
  C.putContext (over C.contextBlocks (<> [C.Text Nothing C.User "parent-curated" []]) current)
  C.setNextModel "parent-curated-model"
  Right captured <- checkpoint "curated parent snapshot"
  idle <- mapM (\name -> spawnSubagent (ForkCtx captured) SameDir
    ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
      { spawnModel = Just (Literal "child-preparation-model"), spawnLabel = Just name }))
    ["first", "second"]
  workers <- mapM (\admitted -> case admitted of
    Left issue -> error (show issue)
    Right actor -> do
      Right reply <- request @Text actor ("specialize this child" :: Text) defaultRequestOptions
      pure reply) idle
  pure True
