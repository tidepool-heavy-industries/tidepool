do
  view <- currentScopeView
  fit <- rowFit Nothing (IdentifierRef "" "absentBinding" ValueIdentifier)
  typed <- typeOfExpression Nothing "1 + 2"
  matches <- typeSearch Nothing "Int"
  case (view, fit, typed, matches) of
    (Right _, Right _, Right _, Right _) -> pure ()
    _ -> error "a compiler query was refused"
