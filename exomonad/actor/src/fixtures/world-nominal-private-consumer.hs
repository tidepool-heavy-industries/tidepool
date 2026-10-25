privateNominal <- pure (id nominal)
privateNominalWitness <- if privateNominal == nominal then pure (41 :: Int) else error "private successor changed the retained nominal value"
