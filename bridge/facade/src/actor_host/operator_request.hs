import qualified Tidepool.Agent.Contract as A
Right workerCapture <- checkpoint "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "operator-worker" })
Right answer <- request @Int worker (41 :: Int) defaultRequestOptions
