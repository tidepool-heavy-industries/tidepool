{-# LANGUAGE DataKinds, OverloadedStrings #-}
module AsyncContext where
import Control.Monad.Freer (Eff, send)
import Tidepool.Effects.Core (ContextReadWrite (..))

-- The ordinary notebook row cannot emit the synchronous effect.
invalid :: Eff '[] ()
invalid = send (SetNextModelWith "executor")
