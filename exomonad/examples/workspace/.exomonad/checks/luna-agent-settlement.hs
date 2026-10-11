joinedScoutSettlements <- await
  ((,) <$> settlement compilerRequestHandle <*> settlement lookupRequestHandle)
case joinedScoutSettlements of
  Left observationFailure ->
    display ("Await failed: " <> T.pack (show observationFailure))
  Right (compilerSettlement, lookupSettlement) ->
    display (T.pack (show (compilerSettlement, lookupSettlement)))
