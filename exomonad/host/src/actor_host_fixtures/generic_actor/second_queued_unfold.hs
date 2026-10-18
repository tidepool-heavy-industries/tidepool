import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import qualified Tidepool.Model.Selection as Selection
import qualified Tidepool.Internal.ModelControl as Control

let fixtureOpenAIEffort options = case Selection.modelSelection currentModel of
      Control.ModelSelectionEnvelope identity key _ -> Selection.openAI identity key options

let laterDomainTask = "Implement the domain component from its captured plan and return its typed report." :: Text
let laterReviewTask = "Inspect the captured review plan and return an independent typed review." :: Text
Right laterWaveContext <- checkpoint currentModel "later-wave-context"
Right laterDomainAgent <- spawnSubagent
  (ForkCtx laterWaveContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "domain"
    , spawnLifetime = ActorOwned
    , spawnModel = fixtureOpenAIEffort (Selection.OpenAIOptions Selection.OpenAILow)
    , spawnInstructions = Just laterDomainTask
    })
Right laterReviewAgent <- spawnSubagent
  (ForkCtx laterWaveContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "review"
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just laterReviewTask
    })
Right laterDomainRequest <- request @Report laterDomainAgent domainPlan
  (defaultRequestOptions { requestLabel = Just "domain" })
Right laterReviewRequest <- request @Review laterReviewAgent reviewPlan
  (defaultRequestOptions { requestLabel = Just "review" })
let otherWorkers = (laterDomainRequest, laterReviewRequest)
