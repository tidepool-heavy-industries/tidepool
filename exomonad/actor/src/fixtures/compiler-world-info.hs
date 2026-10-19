frontendAnswer <- do
  answer <- info (NameQuery (PublicModule "WorldQuestion") ValueName "known")
  pure $ case answer of
    Right details ->
      ( identifierName (inspectedIdentifier details)
      , provenanceFingerprint (identifierProvenance details)
      )
    Left _ -> ("query-refused", "query-refused")
