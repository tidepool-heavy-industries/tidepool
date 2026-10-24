class OpacityDefault value where
  opacityDefault :: value -> Int
  opacityDefault _ = opacitySeed
