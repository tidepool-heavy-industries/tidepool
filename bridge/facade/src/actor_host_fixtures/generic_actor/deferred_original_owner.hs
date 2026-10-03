originalOwnerWorker <- unfoldDeferred (batch "original-owner" "deferred") $
  child (withLifetime ActorOwned (researching @DeferredReply currentCheckout
    (assignment [label|original-owner-child|] (DeferredInput 41))))
