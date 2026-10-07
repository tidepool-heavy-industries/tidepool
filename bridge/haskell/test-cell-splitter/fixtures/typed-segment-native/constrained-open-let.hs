seed <- pure (3 :: Int)
let pair value = (seed, value)
answer <- pure (pair True)
