let Right requestLabel = Agents.labelFromText "publication-refusal"
response <- do
  pending <- Agents.request @Int nativeAgent (Agents.assignment requestLabel ())
  detached <- detachRequest pending
  case detached of
    Left _ -> error "original request detachment refused"
    Right () -> pure pending
pure True
