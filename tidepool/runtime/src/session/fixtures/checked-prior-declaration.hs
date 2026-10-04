data PriorNominal = PriorNominal Int

class PriorClass a where
  priorNumber :: a -> Int

instance PriorClass PriorNominal where
  priorNumber (PriorNominal value) = value

type family PriorPayload flag

type instance PriorPayload Int = PriorNominal
