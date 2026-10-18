import qualified Tidepool.Agent.Contract as A
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options
let parentOnly = 41 :: Int
Right captured <- checkpoint currentModel "model context"
Right exactActor <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAILow), spawnLabel = Just "exact" })
Right selectedActor <- spawnSubagent (FreshCtx "Focused packet: selected") (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAIMedium), spawnLabel = Just "selected" })
Right exact <- request @Text exactActor ("exact" :: Text) defaultRequestOptions
Right selectedWorker <- request @Text selectedActor ("selected" :: Text) defaultRequestOptions
let workers = (exact, selectedWorker)
