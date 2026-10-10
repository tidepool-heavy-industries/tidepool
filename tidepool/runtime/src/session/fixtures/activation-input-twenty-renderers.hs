_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "renderer-input" }
  mapM_ (\(value :: Int) -> do
    _ <- Agents.request @() target value options
    pure ()) [1..20]
