let perfSamples = [1, 2, 3] :: [Int]
let perfAction = (pure (sum perfSamples) :: Eff effects Int)
_ <- display (sum perfSamples)
