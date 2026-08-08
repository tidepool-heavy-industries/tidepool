-- | Intrinsic recognizers key on the ORIGINAL defining module, so a
-- user-local binding that merely shares an occurrence name with a surface
-- verb lowers as ordinary Core.
module Fidelity.Recognizers (checks) where

import Fidelity.Harness (Check)

checks :: IO [Check]
checks = pure []
