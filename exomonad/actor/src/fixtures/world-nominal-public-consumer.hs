publicNominal <- pure (id nominal)
publicNominalWitness <- case capturedNominal () of
  CapturedNominal original -> if publicNominal == original && publicNominal == privateNominal && privateNominalWitness == (41 :: Int) then pure (42 :: Int) else error "promotion changed the retained nominal value"
