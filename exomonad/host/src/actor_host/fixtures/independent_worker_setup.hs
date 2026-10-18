import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options
Right peerActor <- spawnSubagent (FreshCtx "First independent worker") (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel workspaceAgentSpec) { spawnLifetime = RunOwned, spawnModel = fixtureOpenAI "sol" (Selection.OpenAIOptions Selection.OpenAILow), spawnLabel = Just "worker" })
Right peer <- request @Text peerActor ("First independent worker" :: Text) defaultRequestOptions
