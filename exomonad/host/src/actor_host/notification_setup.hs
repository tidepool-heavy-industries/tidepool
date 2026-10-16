import qualified Tidepool.Agent.Contract as A
Right workerCapture <- checkpoint "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "notification-recipient" })
Right answer <- request @Text worker ("original assignment" :: Text) defaultRequestOptions
