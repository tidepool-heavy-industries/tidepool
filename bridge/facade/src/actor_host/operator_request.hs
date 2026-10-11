import qualified Tidepool.Agent.Contract as A
Right worker <- spawnSubagent FreshCtx SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "operator-worker" })
Right answer <- request @Int worker (41 :: Int) defaultRequestOptions
