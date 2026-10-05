_ <- do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (P.error . P.show) id (Agents.labelFromText "package-input")
  _ <- Agents.request @() target (Agents.assignment requestLabel (42 :: Int))
  pure ()
