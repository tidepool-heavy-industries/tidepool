import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)

let laterDomainTask = "Implement the domain component from its captured plan and return its typed report." :: Text
let laterReviewTask = "Inspect the captured review plan and return an independent typed review." :: Text
Right laterWaveContext <- checkpoint "later-wave-context"
Right laterDomainAgent <- spawnSubagent
  (ForkCtx laterWaveContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "domain"
    , spawnLifetime = ActorOwned
    , spawnEffort = Just Low
    , spawnInstructions = Just laterDomainTask
    })
Right laterReviewAgent <- spawnSubagent
  (ForkCtx laterWaveContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "review"
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just laterReviewTask
    })
Right laterDomainRequest <- request @Report laterDomainAgent domainPlan
  (defaultRequestOptions { requestLabel = Just "domain" })
Right laterReviewRequest <- request @Review laterReviewAgent reviewPlan
  (defaultRequestOptions { requestLabel = Just "review" })
let otherWorkers = (laterDomainRequest, laterReviewRequest)
