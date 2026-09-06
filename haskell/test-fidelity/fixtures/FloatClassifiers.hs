module FloatClassifiers where
classifiers :: Double -> Float -> [Bool]
classifiers d f = [isNaN d, isInfinite d, isNegativeZero d,
                   isNaN f, isInfinite f, isNegativeZero f]
