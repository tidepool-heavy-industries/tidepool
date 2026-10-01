module RightFixity where
__result :: IO (Int -> Int -> Int)
__result = do
  let { infixr 5 `minus`; minus = (-) :: Int -> Int -> Int
      ; nested = let { infixl 2 `minus`; minus = (+) :: Int -> Int -> Int } in 4 `minus` 3
      ; result = nested }
  let { __tidepool_checked_annotation_0 :: Int -> Int -> Int
      ; __tidepool_checked_annotation_0 = minus }
  pure (__tidepool_checked_annotation_0)
