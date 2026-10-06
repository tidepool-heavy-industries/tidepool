_ <- do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (P.error . P.show) id (Agents.labelFromText "retained-prefix-input")
  _ <- Agents.request @() target (Agents.assignment requestLabel (Original.make previewPrefix))
  pure ()
