_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "opaque-input" }
  _ <- Agents.request @() target originalInput options
  pure ()
