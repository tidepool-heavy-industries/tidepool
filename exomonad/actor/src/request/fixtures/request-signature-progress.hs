_ <- do
  let target = Ref.internalAgentRef 17 1
  _ <- Agents.request @Int target True Agents.defaultRequestOptions
  _ <- Agents.requestWithProgress @(Maybe Bool) @Int target LT Agents.defaultRequestOptions
  pure ()
