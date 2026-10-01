let leaves = "leaves" :: ForkGroupLabel
let leaf = [label|leaf|]
nested <- unfoldDeferred (subgroup leaves) (child (withLifetime ActorOwned (coding @Text currentCheckout (assignment leaf ("custody-leaf-reply" :: Text)))))
