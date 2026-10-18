import qualified Tidepool.Agent.Contract as A
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection
import qualified Data.Text as T

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options
let parentOnly = 41 :: Int
Right captured <- checkpoint currentModel "model context"
exactAdmission <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAILow), spawnLabel = Just "exact" })
exactActor <- case exactAdmission of { Left refusal -> error (T.pack (show refusal)); Right actor -> pure actor }
selectedAdmission <- spawnSubagent (FreshCtx "Focused packet: selected") (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAIMedium), spawnLabel = Just "selected" })
selectedActor <- case selectedAdmission of { Left refusal -> error (T.pack (show refusal)); Right actor -> pure actor }
Right exact <- request @Text exactActor ("exact" :: Text) defaultRequestOptions
Right selectedWorker <- request @Text selectedActor ("selected" :: Text) defaultRequestOptions
let workers = (exact, selectedWorker)
