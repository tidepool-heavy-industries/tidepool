{-# LANGUAGE ImplicitParams #-}
import qualified Data.List as Imported
  (reverse)

implicitTotal :: (?offset :: Int) => Int -> Int
implicitTotal n = n + ?offset

answer <- pure (let ?offset = 4 in implicitTotal 3)
Imported.reverse [answer, 9] == [9, 7]
