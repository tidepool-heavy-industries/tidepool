{-# LANGUAGE DataKinds, OverloadedStrings #-}
module AsyncContext where
import Control.Monad.Freer (Eff, send)
import Tidepool.Effects.Core (ContextReadWrite (..), Model (CustomModel), Backend (OpenAIPlan))

-- The ordinary notebook row cannot emit the synchronous effect.
invalidModel :: Eff '[] ()
invalidModel = send (SetNextModelWith (CustomModel OpenAIPlan "executor"))
