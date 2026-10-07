-- Internal GHC component input: the resident ordered parser intentionally
-- refuses local fixities, while the existing standalone parser retains them.
let { infixr 4 `minus`; minus :: Int -> Int -> Int; minus x y = x - y }
let answer = 37 :: Int
