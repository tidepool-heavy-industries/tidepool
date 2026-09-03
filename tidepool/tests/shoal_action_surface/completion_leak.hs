{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeOperators #-}

module ShoalCompletionLeak where

import Control.Monad.Freer (Eff)
import Prelude
import Tidepool.Actors.Shoal

result :: Int -> Eff (Complete Int ': ActorEffects) ()
result = complete
