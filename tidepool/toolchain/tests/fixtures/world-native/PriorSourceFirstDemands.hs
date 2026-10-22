priorSeed <- pure (41 :: Int)
data PriorPlainMarker = PriorPlainMarker
priorPlain <- pure (Module.plainNumber (41 :: Int))
data PriorTextMarker = PriorTextMarker
priorText <- pure (Module.textNumber "forty-one")
