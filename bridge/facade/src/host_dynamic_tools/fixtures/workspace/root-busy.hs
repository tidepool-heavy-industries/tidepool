let busyLabel = [label|busy|]
busyWork <- unfoldDeferred (batch campaign group) (child @Text (withLifetime ActorOwned (coding projectHead (assignment busyLabel ("fixture-busy" :: Text)))))
