let Right forwardedLabel = requestLabel "questions"
forwarding <- followAttention updates (ProgressCursor 0) (\questions -> request @Text (forkedActor consumer) forwardedLabel questions >> pure ())
