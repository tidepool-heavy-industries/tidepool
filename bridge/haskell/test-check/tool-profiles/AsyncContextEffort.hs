{-# LANGUAGE DataKinds #-}
module AsyncContextEffort where

import Control.Monad.Freer (Eff)
import qualified Tidepool.Agent.Context as C

-- The ordinary notebook row cannot emit the synchronous effect.
invalid :: Eff '[] ()
invalid = C.setNextEffort C.High
