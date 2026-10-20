do
  view <- currentScopeView
  scope <- scopeBindings Nothing
  exports <- moduleExports Nothing "Data.Maybe"
  fit <- rowFit Nothing (IdentifierRef "" "absentBinding" ValueIdentifier)
  typed <- typeOfExpression Nothing "1 + 2"
  matches <- typeSearch Nothing "Int"
  case (view, scope, exports, fit, typed, matches) of
    (Right _, Right _, Right _, Right _, Right _, Right _) -> pure ()
    _ -> error "a compiler query was refused"
