import qualified Tidepool.Agent.Contract as A
Right worker <- spawnSubagent (FreshCtx "Reply to the operator's typed request.") SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "operator-worker" })
Right answer <- request @Int worker (41 :: Int) defaultRequestOptions
