exhausted <- attemptUnfoldDeferred (subgroup nestedGroup) (child (withLifetime ActorOwned (researching @Text currentCheckout (assignment nestedLabel ()))))
case exhausted of { Left (UnfoldBeginRejected _) -> True; _ -> False }
