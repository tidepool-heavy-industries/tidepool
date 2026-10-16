import qualified Tidepool.Agent.Contract as A

Right originalOwnerSeed <- checkpoint "original-nominal-owner"
Right originalOwnerWorker <- spawnSubagent
  (ForkCtx originalOwnerSeed)
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
    { spawnLabel = Just "original-owner-child"
    , spawnLifetime = ActorOwned
    , spawnInstructions = Just "Inspect the assigned DeferredInput and return its corresponding DeferredReply."
    })
Right originalOwnerRequest <- request @DeferredReply originalOwnerWorker
  (DeferredInput 41)
  (defaultRequestOptions { requestLabel = Just "original-owner-child" })
