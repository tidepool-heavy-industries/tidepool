requestScope <- do
  _ <- Agents.request @() (Ref.internalAgentRef 17 1) () Agents.defaultRequestOptions
  (TidepoolReply.currentRequest :: Eff '[Exomonad.Replies] (TidepoolReply.RequestScope () ()))
