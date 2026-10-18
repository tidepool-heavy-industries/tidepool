import qualified Tidepool.Agent.Contract as A
data ProgressNote = ProgressNote Int (Int -> Int)
Right workerCapture <- checkpoint currentModel "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "source-worker" })
Right (answer, updates) <- requestWithProgress @ProgressNote @Int worker (10 :: Int) defaultRequestOptions
