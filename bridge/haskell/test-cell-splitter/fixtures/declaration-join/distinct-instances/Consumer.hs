module Consumer where

import Join

result :: Int
result = c True + c (0 :: Int)
