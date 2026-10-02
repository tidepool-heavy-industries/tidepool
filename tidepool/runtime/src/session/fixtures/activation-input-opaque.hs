_ <- do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (error . T.pack . P.show) id (Agents.labelFromText "opaque-input")
  _ <- Agents.request @() target (Agents.assignment requestLabel (Original.make 42))
  pure ()
