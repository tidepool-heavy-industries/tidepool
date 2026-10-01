module DefaultFixity where
__result :: IO (Int -> Int -> Int)
__result = do
  let { minus = (-) :: Int -> Int -> Int }
  pure minus
