import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import qualified Tidepool.Model.Selection as Selection
import qualified Tidepool.Internal.ModelControl as Control

let fixtureOpenAIEffort options = case Selection.modelSelection currentModel of
      Control.ModelSelectionEnvelope identity key _ -> Selection.openAI identity key options

let domainLabel = "domain" :: Text
let consumerLabel = "consumer-tests" :: Text
let domainTask = "Implement the domain component and return its typed report." :: Text
let consumerTask = "Inspect the consumer contract and return its typed report." :: Text
let sharedAfterUnfold = ("ready" :: Text)
Right capturedContext <- checkpoint currentModel "unfold-example-context"
Right domainAgent <- spawnSubagent
  (ForkCtx capturedContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just domainLabel
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just domainTask
    })
Right consumerAgent <- spawnSubagent
  (ForkCtx capturedContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just consumerLabel
    , spawnLifetime = ActorOwned
    , spawnModel = fixtureOpenAIEffort (Selection.OpenAIOptions Selection.OpenAIMedium)
    , spawnInstructions = Just consumerTask
    })
Right domainRequest <- request @Report domainAgent domainPlan
  (defaultRequestOptions { requestLabel = Just domainLabel })
Right consumerRequest <- request @Report consumerAgent consumerPlan
  (defaultRequestOptions { requestLabel = Just consumerLabel })
let workers = (domainRequest, consumerRequest)
