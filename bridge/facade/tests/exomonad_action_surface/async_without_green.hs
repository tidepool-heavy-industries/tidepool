{-# LANGUAGE DataKinds #-}

module AsyncWithoutGreen where

import Prelude
import Tidepool.Effects (M)
import qualified Tidepool.Async as Async

result :: M (Async.Async Int)
result = Async.async (pure 41)
