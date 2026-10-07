{-# LANGUAGE BangPatterns #-}
_ <- record 1
let { !() = error "strict closure group"; function () = (3 :: Int) }
_ <- record 2
