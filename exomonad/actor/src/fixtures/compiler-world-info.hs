frontendAnswer <- do
  view <- currentScopeView
  answer <- info (NameQuery (PublicModule "WorldQuestion") ValueName "known")
  case (view, answer) of
    (Right snapshot, Right details) -> do
      before <- scopeBindings (Just snapshot)
      fit <- rowFit (Just snapshot) (inspectedIdentifier details)
      expression <- typeOfExpression (Just snapshot) "queryEcho"
      matches <- typeSearch (Just snapshot) "Int -> Int"
      after <- scopeBindings (Just snapshot)
      finalView <- currentScopeView
      case (before, fit, expression, matches, after, finalView) of
        (Right initialBindings, Right fitted, Right typed, Right found, Right finalBindings, Right finalSnapshot)
          | identifierName (inspectedIdentifier details) == "known"
              && provenanceScope snapshot == CurrentScope
              && identifierModule (inspectedIdentifier details) == "WorldQuestion"
              && identifierNamespace (inspectedIdentifier details) == ValueIdentifier
              && provenanceScope (identifierProvenance details) == PublicModule "WorldQuestion"
              && provenanceGeneration (identifierProvenance details) == provenanceGeneration snapshot
              && provenanceFingerprint (identifierProvenance details) == provenanceFingerprint snapshot
              && (case identifierDeclaration details of
                    ValueDeclaration valueType -> typeCanonical valueType == "Int"
                    _ -> False)
              && fitIdentifier fitted == inspectedIdentifier details
              && fitAvailability fitted == FitAvailable
              && fitProvenance fitted == snapshot
              && expressionSource typed == "queryEcho"
              && expressionDisplay typed == "Int -> Int"
              && expressionAvailability typed == FitAvailable
              && expressionProvenance typed == snapshot
              && (case filter ((== IdentifierRef "WorldQuestion" "queryEcho" ValueIdentifier) . typeMatchReference) (typeMatches found) of
                    [match] -> typeMatchName match == "queryEcho"
                      && typeMatchModule match == Just "WorldQuestion"
                      && typeMatchSignature match == "Int -> Int"
                      && typeMatchQuality match == TypeMatchExact
                      && typeMatchAvailability match == FitAvailable
                    _ -> False)
              && typeMatchesProvenance found == snapshot
              && typeMatchesObserved found == length (typeMatches found)
              && typeMatchesLimit found > 0
              && typeMatchesObserved found <= typeMatchesLimit found
              && IdentifierRef "WorldQuestion" "queryEcho" ValueIdentifier `elem` refs initialBindings
              && refListProvenance initialBindings == snapshot
              && initialBindings == finalBindings
              && finalSnapshot == snapshot -> pure (7 :: Int)
        _ -> error "wrong world query answer"
    _ -> error "world query refused"
