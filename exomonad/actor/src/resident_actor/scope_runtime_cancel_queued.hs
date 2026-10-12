_ <- do
  requested <- Agents.request @Int scopeTarget () Agents.defaultRequestOptions
  queued <- case requested of
    Left failure -> Effects.error ("queued request refused: " <> tshow failure)
    Right pending -> pure pending
  _ <- Reply.cancelRequest queued
  _ <- Reply.cancelRequest scopeNext
  say (tshow True)
