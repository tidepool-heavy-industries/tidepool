{-# LANGUAGE PackageImports #-}
{-# LANGUAGE DataKinds #-}
module CandidateDemandSibling where

import Control.Monad.Freer (Eff)
import "tidepool-resume" Tidepool.Internal.Resume (Settled, settle)
import CandidateDemandInstance (sibling)

result :: Int
result = sibling 40

__prepared :: Settled '[] Int
__prepared = settle (pure result :: Eff '[] Int)
