import qualified Tidepool.Agent.Contract as A
worker <- spawnSubagent (FreshCtx "Reply to the operator's typed request.") SameDir ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "operator-worker" }) >>= either (error . T.pack . show) pure
Right answer <- request @Int worker (41 :: Int) defaultRequestOptions
