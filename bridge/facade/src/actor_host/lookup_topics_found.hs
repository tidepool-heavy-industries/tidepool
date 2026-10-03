let lookupTopicsFound = case LookupApi.lookupResults topics of
      [result] -> case LookupApi.lookupOutcome result of
        LookupApi.LookupFound entries _ -> not (null entries)
        _ -> False
      _ -> False
display lookupTopicsFound
