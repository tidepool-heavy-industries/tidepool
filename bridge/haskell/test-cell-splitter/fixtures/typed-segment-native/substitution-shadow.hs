number <- pure (3 :: Int)
first <- pure (number + 1)
let number = 9 :: Int
second <- pure (number + first)
