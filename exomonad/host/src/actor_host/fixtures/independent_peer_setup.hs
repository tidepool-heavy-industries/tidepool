import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options
Right observerActor <- spawnSubagent (FreshCtx "Retain the exact peer handle for followup") (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel workspaceAgentSpec) { spawnLifetime = RunOwned, spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAILow), spawnLabel = Just "observer" })
Right peerObserver <- request @Text observerActor (responseActor peer) defaultRequestOptions
