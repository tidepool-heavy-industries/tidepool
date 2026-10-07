{-# LANGUAGE BangPatterns #-}
_ <- segmentRecord 1
let { !() = error "strict closure group"; function () = (3 :: Int) }
_ <- segmentRecord 2
