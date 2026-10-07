import qualified Tidepool.Agent.Contract as A
Right workerCapture <- checkpoint "typed worker fixture"
Right first <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "roster-first" })
Right second <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "roster-second" })
Right firstAnswer <- request @Int first (10 :: Int) defaultRequestOptions
Right secondAnswer <- request @Int second (20 :: Int) defaultRequestOptions
