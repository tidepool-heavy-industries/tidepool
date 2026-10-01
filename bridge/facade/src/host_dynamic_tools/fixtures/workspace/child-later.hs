let laterLabel = [label|later|]
laterWork <- unfoldDeferred (batch campaign group) (child @Text (withLifetime ActorOwned (coding currentCheckout (assignment laterLabel ("fixture-later" :: Text)))))
