priorSeed <- pure (41 :: Int)
data PriorPlainMarker = PriorPlainMarker
priorPlain <- pure (PriorSourceBodies.plainNumber (41 :: Int))
data PriorTextMarker = PriorTextMarker
priorText <- pure (PriorSourceBodies.textNumber "forty-one")
