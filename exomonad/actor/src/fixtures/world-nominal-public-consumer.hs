publicNominal <- pure (id nominal)
publicNominalWitness <- if publicNominal == privateNominal && show publicNominal == "Token (Leaf 37)" && privateNominalWitness == (41 :: Int) then pure (42 :: Int) else error "promotion changed the retained nominal value"
