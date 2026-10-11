_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "shared-display" }
  mapM_ (\(value :: Int) -> do
    _ <- Agents.request @() target value options
    pure ()) [41, 42]
  error "parent failed after both child requests"
