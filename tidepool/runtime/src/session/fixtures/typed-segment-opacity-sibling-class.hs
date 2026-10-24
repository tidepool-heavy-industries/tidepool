class OpacityPair value where
  opacityPrimary :: value -> Int
  opacitySecondary :: value -> Int
  opacityPrimary value = opacitySecondary value + opacitySeed
