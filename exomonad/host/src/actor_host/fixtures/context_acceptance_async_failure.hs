import qualified Tidepool.Agent.Contract as A
do
  Right captured <- checkpoint currentModel "failure snapshot"
  Right worker <- spawnSubagent (ForkCtx captured) SameDir
    ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies]))
      { spawnLabel = Just "survives-parent-failure" })
  Right reply <- request @Text worker ("retained request" :: Text) defaultRequestOptions
  error "intentional async parent failure" >> pure True
