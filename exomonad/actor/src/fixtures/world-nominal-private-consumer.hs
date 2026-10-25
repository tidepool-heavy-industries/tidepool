data CapturedNominal = CapturedNominal Nominal.Token
capturedNominal :: () -> CapturedNominal
capturedNominal () = CapturedNominal nominal
privateNominal <- pure (id nominal)
privateNominalWitness <- case capturedNominal () of
  CapturedNominal original -> if privateNominal == original then pure (41 :: Int) else error "private successor changed the retained nominal value"
