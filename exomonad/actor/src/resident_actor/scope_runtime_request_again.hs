scopeNext <- do
  requested <- Agents.request @Int scopeTarget () Agents.defaultRequestOptions
  case requested of
    Left failure -> Effects.error ("second request refused: " <> tshow failure)
    Right pending -> pure pending
_ <- say (tshow True)
