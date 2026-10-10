_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions
  _ <- Agents.request @() target (Original.make 31) options
  _ <- Agents.requestWithProgress @(Maybe Input) @() target (Original.make 42) options
  pure ()
