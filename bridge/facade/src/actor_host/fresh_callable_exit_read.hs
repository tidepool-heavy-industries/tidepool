firstExit <- awaitExit callableExitSuccessor
secondExit <- pollExit callableExitSuccessor
display (case (firstExit, secondExit) of
  (Completed (n, f), Just (Completed (m, g))) -> (n, f 1, f 10, m, g (-2))
  _ -> error "exact callable successor did not complete")
