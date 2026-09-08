let Right statusLabel = requestLabel "source-status"
collection <- followAttentionSources [("left", leftProgress), ("right", rightProgress)] (\state -> request @Text (forkedActor consumer) statusLabel state >> pure ())
