do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "display-prefix" }
  _ <- Agents.request @() target (41 :: Int) options
  pure ()
let unrelatedDisplayValue = (7 :: Int)
do
  let target = Ref.internalAgentRef 17 1
      options = Agents.defaultRequestOptions { Agents.requestLabel = Just "display-prefix" }
  _ <- Agents.request @() target (42 :: Int) options
  pure ()
