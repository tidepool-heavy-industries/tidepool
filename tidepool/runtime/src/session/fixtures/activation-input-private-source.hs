{-# NOINLINE privateIncrement #-}
privateIncrement :: Int -> Int
privateIncrement n = n + 41

_ <- do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (P.error . P.show) id (Agents.labelFromText "private-source-input")
  _ <- Agents.request @() target (Agents.assignment requestLabel privateIncrement)
  pure ()
