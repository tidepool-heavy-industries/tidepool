Right originalOwnerSeed <- checkpoint "original-nominal-owner"
originalOwnerWorker <- unfoldDeferred (batch "original-owner" "checkpoint") $
  child (withContext (fromCheckpoint originalOwnerSeed)
    (withLifetime ActorOwned (narrowed @'[Replies] @DeferredReply knownEffects
      (inspectionPolicy currentCheckout)
      (assignment [label|original-owner-child|] (DeferredInput 41)))))
