{-# NOINLINE privateIncrement #-}
privateIncrement :: Int -> Int
privateIncrement n = n + 41

_ <- do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "private-source-input" }
  _ <- Agents.request @() target privateIncrement options
  pure ()
