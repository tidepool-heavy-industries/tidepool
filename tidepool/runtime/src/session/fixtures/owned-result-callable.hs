_ <- do
  let target = Ref.internalAgentRef 17 1
  _ <- Agents.request @(Int -> Int) target () Agents.defaultRequestOptions
  pure ()
