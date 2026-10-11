data Kept = Kept Int

class KeptClass value where
  keptNumber :: value -> Int

instance KeptClass Kept where
  keptNumber (Kept value) = value + 1

type family KeptPayload flag

type instance KeptPayload Bool = Int

data Hidden = Hidden Int

class HiddenClass value where
  hiddenNumber :: value -> Int

instance HiddenClass Hidden where
  hiddenNumber (Hidden value) = value + 100

type family HiddenPayload flag

type instance HiddenPayload Bool = Int
