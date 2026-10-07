_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "retained-prefix-input" }
  _ <- Agents.request @() target (Original.make previewPrefix) options
  pure ()
