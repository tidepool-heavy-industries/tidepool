do
  agents <- R.call (readAgents (R.client groupStore)) ()
  outcomes <- mapM stopAgent agents
  if all (\outcome -> case outcome of { StoppedNow -> True; AlreadyStopped -> True; _ -> False }) outcomes
    then display True
    else display outcomes
