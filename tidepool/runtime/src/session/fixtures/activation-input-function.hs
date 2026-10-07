_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "function-input" }
  _ <- Agents.request @() target (\(n :: Int) -> n + 1) options
  _ <- Agents.request @() target (\(n :: Int) -> n + 2) options
  pure ()
