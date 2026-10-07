import qualified Tidepool.Agent.Contract as A
let parentOnly = 41 :: Int
Right captured <- checkpoint "model context"
Right exactActor <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = Just (Literal "gpt-6-sol"), spawnLabel = Just "exact" })
Right selectedActor <- spawnSubagent (FreshCtx "Focused packet: selected") (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = Just (Literal "gpt-6-sol"), spawnEffort = Just Medium, spawnLabel = Just "selected" })
Right exact <- request @Text exactActor ("exact" :: Text) defaultRequestOptions
Right selectedWorker <- request @Text selectedActor ("selected" :: Text) defaultRequestOptions
let workers = (exact, selectedWorker)
