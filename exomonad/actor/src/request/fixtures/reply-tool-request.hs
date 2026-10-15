_ <- do
  let target = Ref.internalAgentRef 17 1
  _ <- Agents.requestWithReplyTool @Int target (41 :: Int) Agents.defaultRequestOptions
  pure ()
