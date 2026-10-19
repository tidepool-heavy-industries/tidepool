joinedScoutSettlements <- await
  ((,) <$> settlement compilerRequestHandle <*> settlement introspectionRequestHandle)
case joinedScoutSettlements of
  Left observationFailure ->
    display ("Await failed: " <> T.pack (show observationFailure))
  Right (compilerSettlement, introspectionSettlement) ->
    display (T.pack (show (compilerSettlement, introspectionSettlement)))
