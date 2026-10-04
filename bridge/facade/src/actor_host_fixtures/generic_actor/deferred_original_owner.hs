originalOwnerWorker <- unfoldDeferred (batch "original-owner" "deferred") $
  child (withLifetime ActorOwned (narrowed @'[Replies] @DeferredReply knownEffects
    (inspectionPolicy currentCheckout)
    (assignment [label|original-owner-child|] (DeferredInput 41))))
