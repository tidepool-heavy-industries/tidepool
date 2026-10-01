let nestedGroup = "nested" :: ForkGroupLabel
let nestedLabel = [label|leaf|]
nested <- unfoldDeferred (subgroup nestedGroup) (child (withLifetime ActorOwned (researching @Text currentCheckout (assignment nestedLabel ()))))
