_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "package-input" }
  _ <- Agents.request @() target (42 :: Int) options
  pure ()
