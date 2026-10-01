module CapturedFixity where
__result :: IO (() -> Int)
__result = do
  let { __observation = (\() -> let { infixr 5 `minus`; minus = (-) :: Int -> Int -> Int } in 10 `minus` 3 `minus` 1) }
  pure __observation
