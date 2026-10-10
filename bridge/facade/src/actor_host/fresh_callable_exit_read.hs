firstExit <- awaitExit callableExitSuccessor
secondExit <- pollExit callableExitSuccessor
display (case (firstExit, secondExit) of
  (Completed (n, f), Just (Completed (m, g))) ->
    n == 41 && f 1 == 42 && f 10 == 51 && m == 41 && g (-2) == 39
  _ -> False)
