publicNominal <- pure (id nominal)
publicNominalWitness <- case capturedNominal () of
  CapturedNominal original -> if publicNominal == original && publicNominal == privateNominal && show original == "Token (Leaf 37)" && privateNominalWitness == (41 :: Int) then pure (42 :: Int) else error "promotion changed the retained nominal value"
