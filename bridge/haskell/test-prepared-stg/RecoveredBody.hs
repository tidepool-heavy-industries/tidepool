{-# LANGUAGE DeriveFoldable #-}

module RecoveredBody where

import Prelude (Foldable, Int, fst, sum)

-- Keep the imported package function visible at -O0 so the test can recover
-- the exact 'fst' Id rather than selecting a name reconstructed from output.
caller :: Int
caller = fst (1, ())

data Box a = Box a deriving Foldable

foldableCaller :: Int
foldableCaller = sum (Box 42)
