-- | Probe C: import a quoter-record module (no pragma, no splice),
-- next to a bare Double literal binding.
module M8ImportOnly where

import UpperQQ (upper)

keep :: ()
keep = upper `seq` ()

litd3 :: Double
litd3 = -2.5
