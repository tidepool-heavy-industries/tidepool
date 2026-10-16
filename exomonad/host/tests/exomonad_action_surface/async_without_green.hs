{-# LANGUAGE DataKinds #-}

module AsyncWithoutGreen where

import Prelude
import Control.Monad.Freer (Eff)
import qualified Tidepool.Async as Async

result :: Eff '[] (Async.Async Int)
result = Async.async (pure 41)
