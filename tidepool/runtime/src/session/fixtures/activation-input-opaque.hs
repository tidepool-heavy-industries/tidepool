_ <- do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (P.error . P.show) id (Agents.labelFromText "opaque-input")
  _ <- Agents.request @() target (Agents.assignment requestLabel (Original.make 42))
  pure ()
