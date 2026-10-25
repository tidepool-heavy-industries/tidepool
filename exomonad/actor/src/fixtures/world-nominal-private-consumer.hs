privateNominal <- pure (id nominal)
privateNominalWitness <- if privateNominal == nominal && show privateNominal == "Token (Leaf 37)" then pure (41 :: Int) else error "private successor changed the retained nominal value"
