-- | Probe B: NO quasi-quoter module, no splices — just a direct
-- template-haskell import next to a Double binding. Isolates whether
-- the clz# poisoning comes from template-haskell being in the graph
-- at all, independent of our QQ modules.
module M7THOnly where

import Language.Haskell.TH.Quote (QuasiQuoter (..))

keepQQ :: QuasiQuoter -> QuasiQuoter
keepQQ x = x

litd2 :: Double
litd2 = -2.5
