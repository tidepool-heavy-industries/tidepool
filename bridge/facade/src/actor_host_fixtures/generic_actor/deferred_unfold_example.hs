import qualified Tidepool.Agent.Contract as A

let domainLabel = "domain" :: Text
let consumerLabel = "consumer-tests" :: Text
let domainTask = "Implement the domain component and return its typed report." :: Text
let consumerTask = "Inspect the consumer contract and return its typed report." :: Text
let sharedAfterUnfold = ("ready" :: Text)
Right capturedContext <- checkpoint "unfold-example-context"
Right domainAgent <- spawnSubagent
  (ForkCtx capturedContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just domainLabel
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just domainTask
    })
Right consumerAgent <- spawnSubagent
  (ForkCtx capturedContext)
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just consumerLabel
    , spawnLifetime = ActorOwned
    , spawnEffort = Just Medium
    , spawnInstructions = Just consumerTask
    })
Right domainRequest <- request @Report domainAgent domainPlan
  (defaultRequestOptions { requestLabel = Just domainLabel })
Right consumerRequest <- request @Report consumerAgent consumerPlan
  (defaultRequestOptions { requestLabel = Just consumerLabel })
let workers = (domainRequest, consumerRequest)
