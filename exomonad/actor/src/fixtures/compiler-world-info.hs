frontendAnswer <- do
  view <- currentScopeView
  answer <- info (NameQuery (PublicModule "WorldQuestion") ValueName "known")
  case (view, answer) of
    (Right snapshot, Right details)
      | identifierName (inspectedIdentifier details) == "known"
          && provenanceScope snapshot == CurrentScope
          && identifierModule (inspectedIdentifier details) == "WorldQuestion"
          && identifierNamespace (inspectedIdentifier details) == ValueIdentifier
          && provenanceScope (identifierProvenance details) == PublicModule "WorldQuestion"
          && provenanceGeneration (identifierProvenance details) == provenanceGeneration snapshot
          && provenanceFingerprint (identifierProvenance details) == provenanceFingerprint snapshot
          && (case identifierDeclaration details of
                ValueDeclaration expression -> typeCanonical expression == "Int"
                _ -> False) -> pure (7 :: Int)
    (Right _, Right _) -> error "wrong world query answer"
    _ -> error "world query refused"
