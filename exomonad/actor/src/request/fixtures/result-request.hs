_ <- do
  let target = Ref.internalAgentRef 17 1
  _ <- Agents.request @Int target (41 :: Int) Agents.defaultRequestOptions
  pure ()
