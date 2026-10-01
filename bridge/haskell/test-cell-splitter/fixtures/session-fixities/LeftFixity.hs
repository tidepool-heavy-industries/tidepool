module LeftFixity where
__result :: IO (Int -> Int -> Int)
__result = do
  let { infixl 5 `minus`; minus = (-) :: Int -> Int -> Int }
  let { __tidepool_checked_annotation_0 :: Int -> Int -> Int
      ; __tidepool_checked_annotation_0 = minus }
  pure (__tidepool_checked_annotation_0)
