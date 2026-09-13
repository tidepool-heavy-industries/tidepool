module RecoveredBody where

import Prelude (Int, fst)

-- Keep the imported package function visible at -O0 so the test can recover
-- the exact 'fst' Id rather than selecting a name reconstructed from output.
caller :: Int
caller = fst (1, ())
