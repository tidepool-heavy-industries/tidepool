import qualified Tidepool.Agent.Contract as A
data ProgressNote = ProgressNote Int (Int -> Int)
Right workerCapture <- checkpoint "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "progress-worker" })
Right (answer, updates) <- requestWithProgress @ProgressNote @Int worker (10 :: Int) defaultRequestOptions
let progressWatchLabel = Just "progress-update"
observedUpdate <- watch progressWatchLabel (after updates (ProgressCursor 0))
let combinedLabel = Just "progress-and-answer"
combined <- watch combinedLabel ((,) <$> after updates (ProgressCursor 0) <*> result answer)
let secondLabel = Just "second-cursor"
secondCursor <- watch secondLabel (after updates (ProgressCursor 1))
