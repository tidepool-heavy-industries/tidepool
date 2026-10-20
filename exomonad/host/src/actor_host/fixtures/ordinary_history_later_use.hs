display (ordinaryRead ordinaryToken == 42
  && ordinaryFunction (OrdinaryToken 35) == 42
  && Freer.run ordinaryAction == 42
  && ordinaryMixedRead ordinaryMixedValue == 42)
