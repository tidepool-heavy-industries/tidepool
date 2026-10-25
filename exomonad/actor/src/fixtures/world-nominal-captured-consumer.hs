capturedNominalWitness <- case capturedNominal () of
  CapturedNominal original -> if original == publicNominal && show original == "Token (Leaf 37)" && publicNominalWitness == (42 :: Int) then pure (43 :: Int) else error "declaration capture changed the promoted nominal value"
