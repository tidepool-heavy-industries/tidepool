{-# LANGUAGE PackageImports #-}
{-# LANGUAGE DataKinds #-}
module CandidateDemandWarmer where

import Control.Monad.Freer (Eff)
import "tidepool-resume" Tidepool.Internal.Resume (Settled, settle)
import CandidateDemandFacade
import OptionalSupport ()

result :: Int
result = available (2024 :: Int) + anchor + classAnchor

__prepared :: Settled '[] Int
__prepared = settle (pure result :: Eff '[] Int)
