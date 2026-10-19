frontendAnswer <- do
  answer <- info (NameQuery (PublicModule "WorldQuestion") ValueName "known")
  case answer of
    Right details
      | identifierName (inspectedIdentifier details) == "known"
          && isPrefixOf "compiler-world:"
            (provenanceFingerprint (identifierProvenance details)) ->
              pure (7 :: Int)
    Right _ -> error "wrong world query answer"
    Left _ -> error "world query refused"
