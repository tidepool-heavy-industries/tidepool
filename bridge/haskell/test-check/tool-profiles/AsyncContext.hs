{-# LANGUAGE DataKinds, OverloadedStrings #-}
module AsyncContext where
import Control.Monad.Freer (Eff, send)
import Tidepool.Agent.Context (ForkEffort (High), setNextEffort)
import Tidepool.Effects.Core (ContextReadWrite (..))

-- The ordinary notebook row cannot emit the synchronous effect.
invalidModel :: Eff '[] ()
invalidModel = send (SetNextModelWith "executor")

invalidEffort :: Eff '[] ()
invalidEffort = setNextEffort High
