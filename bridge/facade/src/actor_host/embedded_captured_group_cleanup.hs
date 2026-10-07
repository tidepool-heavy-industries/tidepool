do
  agents <- R.call (readAgents (R.client groupStore)) ()
  outcomes <- mapM stopAgent agents
  display (all (\outcome -> case outcome of { StoppedNow -> True; AlreadyStopped -> True; _ -> False }) outcomes)
