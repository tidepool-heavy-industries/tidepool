let wideGroup = "too-wide" :: ForkGroupLabel
let firstLabel = [label|first|]
let secondLabel = [label|second|]
widthResult <- attemptUnfoldDeferred (subgroup wideGroup) ((,) <$> child (withLifetime ActorOwned (researching @Text currentCheckout (assignment firstLabel ()))) <*> child (withLifetime ActorOwned (researching @Text currentCheckout (assignment secondLabel ()))))
case widthResult of { Left _ -> True; Right _ -> False }
