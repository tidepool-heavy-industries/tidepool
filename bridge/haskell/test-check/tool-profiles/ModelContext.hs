{-# LANGUAGE DataKinds, OverloadedStrings #-}
module ModelContext where
import Control.Monad.Freer (Eff, send)
import Data.Text (Text)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ContextReadWrite (..), ModelCall)
import qualified Tidepool.Model as Model

-- A nested model's hook must not borrow its caller's sync context authority.
spec :: AgentSpec NoTools '[ContextReadWrite, ModelCall]
spec = defaultSpec {afterTool = Just (\_ _ -> send (SetNextModelWith "executor") >> pure NoAnnotation)}

invalid :: Eff '[ContextReadWrite, ModelCall] (Model.ModelResult Text)
invalid = Model.invokeModel (Model.textTurn spec "instructions") "input"
