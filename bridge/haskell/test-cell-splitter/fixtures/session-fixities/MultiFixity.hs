module MultiFixity where
__result :: IO (Int -> Int -> Int, Int -> Int -> Int)
__result = do
  let { infixr 5 `minus`; minus = (-) :: Int -> Int -> Int
      ; infixl 7 `plus`; plus = (+) :: Int -> Int -> Int }
  let { __tidepool_checked_annotation_0 :: Int -> Int -> Int
      ; __tidepool_checked_annotation_0 = minus
      ; __tidepool_checked_annotation_1 :: Int -> Int -> Int
      ; __tidepool_checked_annotation_1 = plus }
  pure (__tidepool_checked_annotation_0, __tidepool_checked_annotation_1)
