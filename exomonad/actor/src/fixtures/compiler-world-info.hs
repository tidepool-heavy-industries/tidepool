frontendAnswer <- do
  view <- currentScopeView
  answer <- info (NameQuery (PublicModule "WorldQuestion") ValueName "known")
  let bounded label value = label ++ ": " ++ take 2048 (show value)
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
              -- Hot queries answer before retained-history preparation:
              -- the inspected row fit is Unknown and bindings are still empty.
              && fitAvailability fitted == FitUnknown
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
              && refsObserved initialBindings == length (refs initialBindings)
              && refsLimit initialBindings > 0
              && refListProvenance initialBindings == snapshot
              && initialBindings == finalBindings
              && finalSnapshot == snapshot -> pure (7 :: Int)
        _ -> error (pack ("wrong world query answer\n" ++ P.unlines
          [ bounded "entry snapshot" snapshot
          , bounded "public info" details
          , bounded "scopeBindings before (counts, provenance, first 8 refs)"
              (fmap (\value -> (refsObserved value, refsLimit value,
                refListProvenance value, take 8 (refs value))) before)
          , bounded "rowFit" fit
          , bounded "typeOfExpression" expression
          , bounded "typeSearch (counts, provenance, first 8 matches)"
              (fmap (\value -> (typeMatchesObserved value, typeMatchesLimit value,
                typeMatchesProvenance value, take 8 (typeMatches value))) matches)
          , bounded "scopeBindings after (counts, provenance, first 8 refs)"
              (fmap (\value -> (refsObserved value, refsLimit value,
                refListProvenance value, take 8 (refs value))) after)
          , bounded "final currentScopeView" finalView
          ]))
    _ -> error (pack ("world query refused\n" ++ P.unlines
      [ bounded "currentScopeView" view
      , bounded "public info" answer
      ]))
