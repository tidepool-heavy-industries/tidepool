Right originalOwnerSeed <- checkpoint "original-nominal-owner"
originalOwnerWorker <- unfoldDeferred (batch "original-owner" "checkpoint") $
  child (withContext (fromCheckpoint originalOwnerSeed)
    (withLifetime ActorOwned (researching @DeferredReply currentCheckout
      (assignment [label|original-owner-child|] (DeferredInput 41)))))
