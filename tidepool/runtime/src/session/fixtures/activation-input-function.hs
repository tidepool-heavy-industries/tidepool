_ <- do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (error . T.pack . P.show) id (Agents.labelFromText "function-input")
  _ <- Agents.request @() target (Agents.assignment requestLabel (\(n :: Int) -> n + 1))
  _ <- Agents.request @() target (Agents.assignment requestLabel (\(n :: Int) -> n + 2))
  pure ()
